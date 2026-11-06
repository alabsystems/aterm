// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::{accounts::proposals::Proposals, database::Change};
use aterm_secrets::commons::bootstrap::Bootstrap;
use aterm_uds::secret::Secret;

thread_local! {
    pub(super) static OBSERVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[test]
fn native_connector_exec_preserves_success_and_failure_status() {
    const CHILD: &str = "ATERM_TEST_NATIVE_CONNECTOR_EXIT";
    const NAME: &str =
        "shared_login::native::tests::native_connector_exec_preserves_success_and_failure_status";
    if let Some(code) = std::env::var_os(CHILD) {
        let code = code.to_str().unwrap().parse::<u8>().unwrap();
        let shell = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
        let mut command = aterm_uds::spawnfd::command(shell);
        command.args(["-c", &format!("echo native-connector-ran; exit {code}")]);
        match replace_with_connector(command) {
            Err(why) => panic!("{why}"),
            Ok(never) => match never {},
        }
    }
    for code in [0, 37] {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", NAME, "--nocapture"])
            .env(CHILD, code.to_string());
        let output = aterm_uds::deadline::ends_within(&mut command, aterm_spec::wait::MUST_HAPPEN);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("native-connector-ran"));
    }
}

#[test]
fn native_connector_exec_failure_remains_an_error() {
    let home = aterm_tempfile::tempdir().unwrap();
    let command = aterm_uds::spawnfd::command(home.path().join("missing-connector"));
    let error = replace_with_connector(command).unwrap_err();
    assert!(error.starts_with("native connector did not start: "));
}

#[test]
fn native_preparation_refuses_unknown_local_policy_before_provider_access() {
    let (_temp, approved, config) = fixture(Provider::Google, false);
    let mut ordinary = Server {
        calls: 0,
        after: |_| {},
    };
    prepare_one(&approved, &config, &mut ordinary, deadline()).unwrap();
    assert_eq!(ordinary.calls, 2);
    let before = std::fs::read(approved.paths.dir.join(RECORDS)).unwrap();
    let mut outcomes = Vec::new();
    for unreadable in [false, true] {
        if unreadable {
            std::fs::remove_file(&config).unwrap();
            std::fs::create_dir(&config).unwrap();
        } else {
            std::fs::write(&config, "[harness\naccounts = 'off'\n").unwrap();
        }
        OBSERVATIONS.with(|count| count.set(0));
        let mut server = Server {
            calls: 0,
            after: |_| {},
        };
        let refused = prepare_one(&approved, &config, &mut server, deadline()).is_err();
        let observations = OBSERVATIONS.with(std::cell::Cell::get);
        outcomes.push((refused, server.calls, observations));
    }
    assert_eq!(outcomes, vec![(true, 0, 0), (true, 0, 0)]);
    assert_eq!(
        std::fs::read(approved.paths.dir.join(RECORDS)).unwrap(),
        before
    );
}

#[test]
fn native_preparation_rechecks_off_before_its_first_credential_observation() {
    let (_temp, approved, config) = fixture(Provider::Meta, false);
    let mut server = Server {
        calls: 0,
        after: |_| {},
    };
    prepare_one(&approved, &config, &mut server, deadline()).unwrap();
    assert_eq!(server.calls, 2);
    assert!(OBSERVATIONS.with(std::cell::Cell::get) > 0);
    let before = std::fs::read(approved.paths.dir.join(RECORDS)).unwrap();
    let mut outcomes = Vec::new();
    for setting in ["enabled = false", "accounts = 'off'", "logins = 'off'"] {
        std::fs::write(&config, format!("[harness]\n{setting}\n")).unwrap();
        OBSERVATIONS.with(|count| count.set(0));
        let mut server = Server {
            calls: 0,
            after: |_| {},
        };
        let refused = prepare_one(&approved, &config, &mut server, deadline()).is_err();
        outcomes.push((
            refused,
            server.calls,
            OBSERVATIONS.with(std::cell::Cell::get),
        ));
    }
    assert_eq!(outcomes, vec![(true, 0, 0); 3]);
    assert_eq!(
        std::fs::read(approved.paths.dir.join(RECORDS)).unwrap(),
        before
    );
}

fn deadline() -> Instant {
    Instant::now() + aterm_spec::wait::MUST_HAPPEN
}

fn successor_fixture() -> (aterm_tempfile::TempDir, Vec<Paths>, PathBuf) {
    native_fleet_fixture(3)
}

fn native_fleet_fixture(members: usize) -> (aterm_tempfile::TempDir, Vec<Paths>, PathBuf) {
    use aterm_secrets::commons::{
        self,
        binding::Pending,
        bootstrap::{Descriptor, Device},
        roster::{Enrollment, Role},
    };
    let temp = aterm_tempfile::tempdir().unwrap();
    let paths: Vec<_> = (1..=members)
        .map(|n| Paths::for_home(&temp.path().join(format!("m{n}"))))
        .collect();
    let names: Vec<_> = (1..=members).map(|n| format!("m{n}")).collect();
    let source = temp.path().join("test.vault");
    client::test_create_unbound(&paths[0], source.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = client::admin_open(&paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    for (n, path) in paths.iter().enumerate().skip(1) {
        let identity = client::machine_identity(path).unwrap();
        doc.add_machine(&format!("m{}", n + 1), &identity.recipient().to_bech32())
            .unwrap();
    }
    let mut policy = Policy::default();
    policy
        .set(Assignment {
            subject: Subject {
                account: [7; 32],
                revision: [8; 32],
                provider: Provider::Google,
                scope: Scope::Preparation,
            },
            warm: vec!["m2".into()],
            automatic: None,
        })
        .unwrap();
    doc.set_entry(&Audience::Everyone, ENTRY, policy.text().as_bytes())
        .unwrap();
    doc.set_entry(
        &Audience::Everyone,
        "native-input",
        br#"{"schema":1,"kind":"api-key","provider":"google","key":"synthetic-key"}"#,
    )
    .unwrap();
    let roster = aterm_secrets::swarm_roster::seed_roster(
        &doc,
        &BTreeMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        &BTreeMap::new(),
    );
    doc.set_entry(
        &Audience::Everyone,
        aterm_secrets::SWARM_ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    doc.set_entry(&Audience::Hosts(names.clone()), commons::ENTRY, b"pending")
        .unwrap();
    client::admin_publish(&paths[0], &admin, &doc).unwrap();
    let admin = client::admin_open(&paths[0]).unwrap();
    let network = [123; 16];
    let mut enrollments = Vec::new();
    for (n, path) in paths.iter().enumerate() {
        let device = Device::open(path).unwrap().unwrap();
        let request = device
            .request(network, admin.commons_authority().unwrap())
            .unwrap();
        let (mut pending, challenge) =
            Pending::issue(&request, network, *request.binding().authority()).unwrap();
        let proof = pending
            .verify(&device.answer(&request, &challenge).unwrap())
            .unwrap();
        enrollments.push(Enrollment::new(format!("m{}", n + 1), proof, Role::Writer));
    }
    let descriptor =
        Descriptor::from_genesis(admin.prepare_commons_genesis(network, enrollments).unwrap())
            .unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Hosts(names), commons::ENTRY, &descriptor.bytes())
        .unwrap();
    client::admin_publish(&paths[0], &admin, &doc).unwrap();
    for (n, path) in paths.iter().enumerate().skip(1) {
        client::join(
            path,
            &client::JoinOptions {
                source: format!(
                    "{}#{}",
                    source.display(),
                    aterm_secrets::b64::encode_padded(&admin.config.signer.unwrap())
                ),
                host: Some(format!("m{}", n + 1)),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
    }
    Proposals::at(paths[0].clone()).write([31;16],vec![Change{collection:"accounts".into(),key:"native".into(),value:Some(json!({"provider":"google","method":"api-key","secret_ref":{"vault":admin.config.name,"entry":"native-input"}}))}]).unwrap();
    let db = Activations::at(paths[0].clone());
    let admin = client::admin_open(&paths[0]).unwrap();
    let request = db
        .intent_configuration(&admin, [7; 32], "native", vec![])
        .unwrap();
    db.issue(&admin, [32; 16], request).unwrap();
    for (n, path) in paths.iter().enumerate() {
        let actor = Bootstrap::required(path)
            .unwrap()
            .request()
            .unwrap()
            .binding()
            .actor();
        let request = db.intent_member_request([7; 32], actor).unwrap().unwrap();
        db.issue(&admin, [n as u8 + 33; 16], request).unwrap();
    }
    let config = temp.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    (temp, paths, config)
}

#[test]
fn native_policy_and_member_follow_real_successors_and_recheck_generation() {
    let (temp, paths, config) = successor_fixture();
    let original = &paths[0];
    let expected = client::config(original).unwrap().unwrap();
    let next = aterm_db::test_complete_writer_successor(
        temp.path(),
        original,
        &paths[1],
        &config,
        deadline(),
    )
    .unwrap();
    assert_eq!(
        aterm_db::application_paths_until(original, deadline()).unwrap(),
        next
    );
    std::fs::remove_file(original.home.join(".secrets").join(ENTRY)).unwrap();
    assert!(
        policy_at(original, &expected, deadline())
            .unwrap()
            .is_some()
    );
    assert_eq!(member(original, deadline()).unwrap(), "m2");
    let before = selection_scope(original, deadline()).unwrap();
    let replacement = Bootstrap::required(&paths[2])
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .clone();
    assert!(
        with_custody(original, &expected, deadline(), |active, _| {
            assert_eq!(*active, next);
            aterm_db::test_complete_writer_successor(
                temp.path(),
                active,
                &paths[2],
                &config,
                deadline(),
            )
            .unwrap();
            Ok(())
        })
        .is_err()
    );
    let selected = aterm_db::application_paths_until(original, deadline()).unwrap();
    assert_eq!(
        Bootstrap::required(&selected)
            .unwrap()
            .request()
            .unwrap()
            .binding(),
        &replacement
    );
    let roster = client::swarm_roster_until(
        &selected,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    .unwrap();
    let expected_label = &roster
        .member_by_recipient(&replacement.recipient().to_bech32())
        .unwrap()
        .label;
    assert_eq!(&member(original, deadline()).unwrap(), expected_label);
    assert_ne!(
        selection_scope(original, deadline()).unwrap()["actor"],
        before["actor"]
    );
    assert!(
        with_custody(
            original,
            &expected,
            Instant::now(),
            |_, _| -> Result<(), String> { panic!("expired read ran") }
        )
        .is_err()
    );
}

#[test]
fn allocator_rebinds_across_verified_generations_but_refuses_foreign_delegation() {
    let (temp, paths, config) = successor_fixture();
    let original = &paths[0];
    let mut admin = client::admin_open(original).unwrap();
    admin
        .doc
        .set_entry(
            &Audience::Everyone,
            ENTRY,
            Policy::default().text().as_bytes(),
        )
        .unwrap();
    client::admin_publish(original, &admin, &admin.doc).unwrap();
    set_policy_mode(&original.home, [7; 32], Vec::new(), true).unwrap();
    client::sync(&paths[1], None).unwrap();
    let mut active = aterm_db::test_complete_writer_successor(
        temp.path(),
        original,
        &paths[1],
        &config,
        deadline(),
    )
    .unwrap();
    let now = Instant::now();
    let expected = client::config(original).unwrap().unwrap();
    for generation in 0..2 {
        let (before, _) = policy_at(original, &expected, deadline()).unwrap().unwrap();
        let subject = before.automatic(Provider::Google).unwrap().unwrap().subject;
        let grant = before
            .automatic(Provider::Google)
            .unwrap()
            .unwrap()
            .automatic
            .as_ref()
            .unwrap()
            .grant;
        allocation::request(original, &before, subject, deadline()).unwrap();
        let admin = client::admin_cached(original).unwrap();
        if generation == 0 {
            assert_eq!(
                allocation::step_at(original, &admin, &config, deadline(), now).unwrap(),
                0
            );
        }
        assert_eq!(
            allocation::step_at(
                original,
                &admin,
                &config,
                deadline(),
                now + Duration::from_secs(60 * (generation + 1))
            )
            .unwrap(),
            1
        );
        client::sync_reader_until(&active, deadline()).unwrap();
        let (after, _) = policy_at(original, &expected, deadline()).unwrap().unwrap();
        let actual = aterm_db::application_lineage_until(original, deadline()).unwrap();
        let delegate = after
            .automatic(Provider::Google)
            .unwrap()
            .unwrap()
            .automatic
            .as_ref()
            .unwrap();
        assert_eq!(delegate.grant, grant);
        assert_eq!(delegate.actor, actual.last().unwrap().actor());
        assert_eq!(&delegate.machine, actual.last().unwrap().machine());
        let view =
            aterm_db::accounts::native_requests::Requests::at_until(original.clone(), deadline())
                .view()
                .unwrap();
        let mut foreign = delegate.clone();
        foreign.machine = [255; 16];
        foreign.actor = [255; 48];
        assert!(!allocation::delegated(
            &foreign,
            &view.local,
            &view.members,
            &actual
        ));
        if generation == 0 {
            client::sync(&paths[2], None).unwrap();
            active = aterm_db::test_complete_writer_successor(
                temp.path(),
                &active,
                &paths[2],
                &config,
                deadline(),
            )
            .unwrap();
        }
    }
}

fn fixture(provider: Provider, intent: bool) -> (aterm_tempfile::TempDir, Approved, PathBuf) {
    let temp = aterm_tempfile::tempdir().unwrap();
    let home = temp.path().join("machine");
    let paths = Paths::for_home(&home);
    client::create(
        &paths,
        temp.path().join("test.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    let value = json!({"schema":1,"kind":"api-key","provider":provider.name(),"key":"synthetic-private-key"});
    doc.set_entry(
        &Audience::Everyone,
        "native-input",
        &aterm_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    // The companion entry is independent of the original pool's schema.
    doc.set_entry(&Audience::Everyone, "ai-pool.toml", b"version = 1\n")
        .unwrap();
    let roster = aterm_secrets::swarm_roster::seed_roster(
        &doc,
        &BTreeMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        &BTreeMap::new(),
    );
    doc.set_entry(
        &Audience::Everyone,
        aterm_secrets::SWARM_ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    let config = home.join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let expected = client::config(&paths).unwrap().unwrap();
    Proposals::at(paths.clone())
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "native".into(),
                value: Some(json!({
                            "provider":provider.name(),"method":"api-key",
                "secret_ref":{"vault":expected.name,"entry":"native-input"}
                        })),
            }],
        )
        .unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let db = Activations::at(paths.clone());
    let actor = Bootstrap::required(&paths)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    let request = if intent {
        db.intent_configuration(&admin, [7; 32], "native", vec![])
            .unwrap()
    } else {
        db.configuration(&admin, [7; 32], "native", actor, vec![])
            .unwrap()
    };
    let (record, _) = db.issue(&admin, [2; 16], request).unwrap();
    let (account, decision) = if intent {
        let (child, _) = db.materialize_next_intent(&admin).unwrap().unwrap();
        (child.request().account, child.id())
    } else {
        ([7; 32], record.id())
    };
    (
        temp,
        Approved {
            paths,
            expected,
            account,
            decision,
        },
        config,
    )
}

struct Server<F> {
    calls: usize,
    after: F,
}

#[test]
fn signed_native_candidates_progress_across_sparse_preparation_passes() {
    for provider in [Provider::Google, Provider::Meta] {
        let (_temp, first, _config) = fixture(provider, false);
        let db = Activations::at(first.paths.clone());
        let admin = client::admin_open(&first.paths).unwrap();
        let actor = Bootstrap::required(&first.paths)
            .unwrap()
            .request()
            .unwrap()
            .binding()
            .actor();
        let request = db
            .configuration(&admin, [8; 32], "native", actor, vec![])
            .unwrap();
        db.issue(&admin, [3; 16], request).unwrap();
        let home = &first.paths.home;
        let start = Instant::now();
        let mut queue = native_keys::PreparationQueue::default();
        let mut picked = Vec::new();
        assert!(
            select_preparation(
                &mut queue,
                home,
                candidates(home, deadline()).unwrap(),
                start,
                &mut |_| false,
            )
            .is_none()
        );
        for seconds in [0, 60, 120, 180] {
            let approved = candidates(home, deadline()).unwrap();
            assert_eq!(approved.len(), 2);
            picked.push(
                select_preparation(
                    &mut queue,
                    home,
                    approved,
                    start + Duration::from_secs(seconds),
                    &mut |_| true,
                )
                .unwrap()
                .account,
            );
        }
        assert_eq!(picked, vec![[7; 32], [8; 32], [7; 32], [8; 32]]);
        let now = start + Duration::from_secs(240);
        let mut offered = Vec::new();
        let selected = select_preparation(
            &mut queue,
            home,
            candidates(home, deadline()).unwrap(),
            now,
            &mut |key| {
                offered.push(key.key.clone());
                key.key == preparation_key([8; 32])
            },
        )
        .unwrap();
        assert_eq!(selected.account, [8; 32]);
        assert_eq!(
            offered,
            [preparation_key([7; 32]), preparation_key([8; 32])]
        );
        assert_eq!(
            select_preparation(
                &mut queue,
                home,
                candidates(home, deadline()).unwrap(),
                now,
                &mut |_| true,
            )
            .unwrap()
            .account,
            [7; 32]
        );
        assert!(
            !first.paths.dir.join(RECORDS).exists(),
            "selection performs no provider preparation"
        );
    }
}
#[test]
fn signed_native_reissue_wakes_a_recent_preparation() {
    let (_temp, first, _config) = fixture(Provider::Google, false);
    let home = &first.paths.home;
    let start = Instant::now();
    let mut queue = native_keys::PreparationQueue::default();
    let selected = select_preparation(
        &mut queue,
        home,
        candidates(home, deadline()).unwrap(),
        start,
        &mut |_| true,
    )
    .unwrap();
    assert_eq!(selected.decision, first.decision);
    assert!(
        select_preparation(
            &mut queue,
            home,
            candidates(home, deadline()).unwrap(),
            start + Duration::from_secs(1),
            &mut |_| true,
        )
        .is_none(),
        "an unchanged decision retains its cooldown"
    );

    let db = Activations::at(first.paths.clone());
    let admin = client::admin_open(&first.paths).unwrap();
    let actor = Bootstrap::required(&first.paths)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    let request = db
        .configuration(&admin, first.account, "native", actor, vec![first.decision])
        .unwrap();
    let (replacement, _) = db.issue(&admin, [19; 16], request).unwrap();
    assert_ne!(replacement.id(), first.decision);
    let replacements = candidates(home, deadline()).unwrap();
    assert_eq!(replacements.len(), 1);
    assert_eq!(replacements[0].decision, replacement.id());
    let selected = select_preparation(
        &mut queue,
        home,
        replacements,
        start + Duration::from_secs(2),
        &mut |_| true,
    )
    .expect("a new signed decision is independently eligible");
    assert_eq!(selected.account, first.account);
    assert_eq!(selected.decision, replacement.id());
    assert!(
        !first.paths.dir.join(RECORDS).exists(),
        "selection starts no provider work"
    );
}

impl<F: FnMut(usize)> native_keys::Probe for Server<F> {
    fn get(
        &mut self,
        provider: Provider,
        key: Option<&[u8]>,
        _: Instant,
    ) -> Result<native_keys::Reply, String> {
        self.calls += 1;
        assert_eq!(key.is_some(), self.calls.is_multiple_of(2));
        (self.after)(self.calls);
        let body = match provider {
            Provider::Google => json!({"models":[{"name":"models/test"}]}),
            Provider::Meta => json!({"object":"list","data":[{"object":"model","id":"test"}]}),
        };
        Ok(native_keys::Reply {
            status: if key.is_some() { 200 } else { 401 },
            body: Secret::from_vec(aterm_json::to_vec(&body).unwrap()),
        })
    }
}

#[test]
fn native_preparation_and_signed_assignment_are_distinct_and_keep_old_pool() {
    for provider in [Provider::Google, Provider::Meta] {
        let (_temp, approved, config) = fixture(provider, false);
        let observed = observation(&approved, deadline()).unwrap();
        assert!(permits(&approved, &observed, &config, deadline()).is_err());
        let mut server = Server {
            calls: 0,
            after: |_| {},
        };
        let access = prepare_one(&approved, &config, &mut server, deadline()).unwrap();
        assert_eq!(server.calls, 2);
        let rows = records(&approved.paths).unwrap();
        assert_eq!(rows[0]["state"].as_str(), Some("access-verified"));
        assert_eq!(rows[0]["current_validity"].as_str(), Some("unknown"));
        assert!(
            !aterm_json::to_string(&rows)
                .unwrap()
                .contains("synthetic-private-key")
        );
        assert!(permits(&approved, &observed, &config, deadline()).is_err());
        set_policy(&approved.paths.home, approved.account, vec!["m1".into()]).unwrap();
        permits(&approved, &observed, &config, deadline()).unwrap();
        let admin = client::admin_open(&approved.paths).unwrap();
        let old = admin
            .doc
            .entry(&Audience::Everyone, "ai-pool.toml")
            .unwrap();
        assert_eq!(old.value.as_slice(), b"version = 1\n");
        assert!(
            aterm_agent::harness::ai_pool::Pool::from_toml(old.value.as_str().unwrap(), "old-pool")
                .is_ok()
        );
        assert!(
            access.matches(
                &current(&approved, &observed, &config, deadline())
                    .unwrap()
                    .input
            )
        );
    }
}

/// A MACHINE THAT SET UP NO ACCOUNT CATALOG HAS NONE TO READ, WHICH IS NO FAILURE: `shared_accounts::
/// read` says so of a home with no Commons (`aterm-db`'s `snapshot_if_declared` tests hold a Commons
/// that declared neither table to the same `None`). CONTROL: a Commons that declared the proposal
/// table and not the import table reads, with its history incomplete and the missing table named.
#[test]
fn a_commons_that_declared_no_account_catalog_reads_as_none() {
    let (temp, paths, _config) = successor_fixture();
    assert_eq!(
        crate::shared_accounts::read(&temp.path().join("no-commons")),
        Ok(None)
    );
    let catalog = crate::shared_accounts::read(&paths[0].home)
        .unwrap()
        .expect("CONTROL: a declared table");
    assert!(!catalog.history_complete, "CONTROL");
    assert!(
        catalog
            .source_errors
            .iter()
            .any(|why| why.starts_with("imports: ")),
        "CONTROL: {:?}",
        catalog.source_errors
    );
}

#[test]
fn native_raw_intake_attaches_reference_without_grant_or_secret_output() {
    let (_temp, approved, _config) = fixture(Provider::Google, false);
    let before = Activations::at(approved.paths.clone())
        .state(approved.account)
        .unwrap();
    let result = input::set(&approved.paths, "native", b"new-synthetic-private-key").unwrap();
    assert_eq!(result["state"].as_str(), Some("attached"));
    assert!(!result.to_string().contains("new-synthetic-private-key"));
    let snapshot = Proposals::at(approved.paths.clone()).snapshot().unwrap();
    let value = &snapshot.cells[&("accounts".into(), "native".into())].candidates[0].value;
    assert_eq!(value["provider"].as_str(), Some("google"));
    assert_eq!(value["method"].as_str(), Some("api-key"));
    let name = value["secret_ref"]["entry"].as_str().unwrap();
    assert!(name.starts_with("native-api-key-"));
    let config = client::config(&approved.paths).unwrap().unwrap();
    let entry = client::read_verified_entry(&approved.paths, &config, name)
        .unwrap()
        .unwrap();
    assert!(Inputs::decode(entry.value().as_slice(), Provider::Google).is_ok());
    assert_eq!(
        Activations::at(approved.paths.clone())
            .state(approved.account)
            .unwrap(),
        before
    );
    assert!(
        observation(&approved, deadline()).is_err(),
        "the old grant cannot follow changed input configuration"
    );
    assert!(!input::pending(&approved.paths).unwrap());
}

#[test]
fn typed_native_key_intake_fills_a_missing_method_without_new_authority() {
    let (_temp, approved, _) = fixture(Provider::Meta, false);
    let proposals = Proposals::at(approved.paths.clone());
    let csv = _temp.path().join("accounts.csv");
    std::fs::write(&csv, "email,meta\nmetadata@example.test,1\n").unwrap();
    let imports = aterm_db::import::commons::Imports::at(approved.paths.clone());
    let result = imports.apply(&csv, [88; 16]).unwrap();
    let before = imports.snapshot().unwrap();
    let selector = format!("import:{}:2:2", result["source_sha256"].as_str().unwrap());
    let result = input::set(&approved.paths, &selector, b"supplied-api-key").unwrap();
    let key = result["key"].as_str().unwrap();
    let snapshot = proposals.snapshot().unwrap();
    let value = &snapshot.cells[&("accounts".into(), key.into())].candidates[0].value;
    assert_eq!(value["method"].as_str(), Some("api-key"));
    assert!(value["secret_ref"].is_object());
    assert_eq!(value["email"].as_str(), Some("metadata@example.test"));
    assert!(
        value["source"]["import"]["value"]
            .as_str()
            .unwrap()
            .ends_with(&selector)
    );
    let after = imports.snapshot().unwrap();
    assert_eq!(after.cells, before.cells);
    assert_eq!(after.operations.len(), before.operations.len());
    assert_eq!(
        Activations::at(approved.paths.clone())
            .state(approved.account)
            .unwrap(),
        aterm_db::accounts::activations::State::Ready(approved.decision)
    );
    let catalog = crate::shared_accounts::read(&approved.paths.home)
        .unwrap()
        .expect("a declared catalog");
    assert!(catalog.rows.iter().any(|row| {
        row.key == key
            && row
                .import_link
                .as_ref()
                .is_some_and(|link| link.ends_with(&selector))
    }));
}

#[test]
fn imported_native_intake_refuses_changed_sources_and_existing_target_edits() {
    for change_source in [false, true] {
        let (temp, approved, _) = fixture(Provider::Google, false);
        let path = temp.path().join("accounts.csv");
        std::fs::write(&path, "email,google\nsource@example.test,1\n").unwrap();
        let imports = aterm_db::import::commons::Imports::at(approved.paths.clone());
        let source = imports.apply(&path, [51; 16]).unwrap();
        let selector = format!("import:{}:2:2", source["source_sha256"].as_str().unwrap());
        if change_source {
            assert!(
                input::set_after_pending(&approved.paths, &selector, b"key", || {
                    imports.apply(&path, [52; 16]).unwrap();
                    Err("simulated stop".into())
                })
                .is_err()
            );
            let before = Proposals::at(approved.paths.clone()).snapshot().unwrap();
            assert!(input::recover(&approved.paths, None).is_err());
            assert_eq!(
                Proposals::at(approved.paths.clone())
                    .snapshot()
                    .unwrap()
                    .cells,
                before.cells
            );
            assert!(input::pending(&approved.paths).unwrap());
        } else {
            let result = input::set(&approved.paths, &selector, b"key").unwrap();
            let key = result["key"].as_str().unwrap();
            let db = Proposals::at(approved.paths.clone());
            let mut value = db.snapshot().unwrap().cells[&("accounts".into(), key.into())]
                .candidates[0]
                .value
                .clone();
            value
                .as_object_mut()
                .unwrap()
                .insert("plan".into(), json!("explicit-edit"));
            db.write(
                [53; 16],
                vec![Change {
                    collection: "accounts".into(),
                    key: key.into(),
                    value: Some(value.clone()),
                }],
            )
            .unwrap();
            assert!(input::set(&approved.paths, &selector, b"another-key").is_err());
            assert!(!input::pending(&approved.paths).unwrap());
            assert_eq!(
                db.snapshot().unwrap().cells[&("accounts".into(), key.into())].candidates[0].value,
                value
            );
        }
    }
}

fn with_intake_store<T>(
    paths: &Paths,
    application: &str,
    action: impl FnOnce(&aterm_db::table::Session<'_>, &mut aterm_db::table::store::Store) -> T,
) -> T {
    let bootstrap = Bootstrap::required(paths).unwrap();
    let session = aterm_db::table::Session::open(&bootstrap).unwrap();
    let scope =
        aterm_db::catalog::Descriptor::application(bootstrap.trust(), "accounts", application, 1)
            .unwrap()
            .scope;
    let directory = aterm_db::catalog::Directory::new(paths, bootstrap.trust());
    let mut store =
        aterm_db::table::store::Store::open(&session, &directory.path(scope).unwrap(), scope)
            .unwrap();
    action(&session, &mut store)
}

fn transfer_intake_history(from: &Paths, to: &Paths, application: &str, receipt: &Value) {
    let id = super::super::digest::<32>(receipt["operation"].as_str().unwrap()).unwrap();
    let wire = with_intake_store(from, application, |session, store| {
        store.wire(session, &id).unwrap().unwrap().to_vec()
    });
    with_intake_store(to, application, |session, store| {
        store.ingest(session, &wire).unwrap()
    });
}

#[test]
fn native_key_intake_waits_for_missing_history_before_staging_and_recovery() {
    use aterm_db::import::commons::Imports;
    use std::cell::Cell;

    for (application, during_pending) in [
        ("imports", false),
        ("imports", true),
        ("proposals", false),
        ("proposals", true),
    ] {
        let (temp, paths, _) = native_fleet_fixture(2);
        let imported = Imports::at(paths[0].clone());
        let peer = Imports::at(paths[1].clone());
        let csv = temp.path().join("selected.csv");
        let other = temp.path().join("other.csv");
        std::fs::write(&csv, "email,google\nselected@example.test,1\n").unwrap();
        std::fs::write(&other, "email,google\nother@example.test,1\n").unwrap();
        let selected = imported.apply(&csv, [70; 16]).unwrap();
        let seed = peer.apply(&other, [71; 16]).unwrap();
        transfer_intake_history(&paths[0], &paths[1], "imports", &selected);
        transfer_intake_history(&paths[1], &paths[0], "imports", &seed);
        let before = Proposals::at(paths[0].clone()).snapshot().unwrap().cells;
        let (parent, child) = if application == "imports" {
            (
                peer.apply(&other, [72; 16]).unwrap(),
                peer.apply(&other, [73; 16]).unwrap(),
            )
        } else {
            let proposals = Proposals::at(paths[1].clone());
            proposals.initialize().unwrap();
            let base = json!({"operation":aterm_codec::hex::encode_lower(
                &before[&("accounts".into(), "native".into())].candidates[0].operation
            )});
            transfer_intake_history(&paths[0], &paths[1], "proposals", &base);
            let change = Change {
                collection: "mailboxes".into(),
                key: "unrelated".into(),
                value: Some(json!({"name":"unrelated"})),
            };
            let parent = proposals.write([72; 16], vec![change.clone()]).unwrap();
            let child = proposals.write([73; 16], vec![change]).unwrap();
            (
                json!({"operation":aterm_codec::hex::encode_lower(&parent.id)}),
                json!({"operation":aterm_codec::hex::encode_lower(&child.id)}),
            )
        };
        let selector = format!("import:{}:2:2", selected["source_sha256"].as_str().unwrap());
        let callback = Cell::new(false);
        if !during_pending {
            transfer_intake_history(&paths[1], &paths[0], application, &child);
        }
        let result = input::set_after_pending(&paths[0], &selector, b"synthetic-key", || {
            callback.set(true);
            if during_pending {
                transfer_intake_history(&paths[1], &paths[0], application, &child);
            }
            Err("stop before publication".into())
        });
        assert!(result.is_err());
        assert_eq!(callback.get(), during_pending);
        assert_eq!(input::pending(&paths[0]).unwrap(), during_pending);
        let snapshot = if application == "imports" {
            imported.snapshot().unwrap()
        } else {
            Proposals::at(paths[0].clone()).snapshot().unwrap()
        };
        assert_eq!(snapshot.withheld, 1);
        assert!(!snapshot.history_complete());
        let config = client::config(&paths[0]).unwrap().unwrap();
        let aterm_secrets::transport::Source::Path(vault) = config.source else {
            panic!("fixture uses a local vault");
        };
        let vault_before = std::fs::read(&vault).unwrap();
        if during_pending {
            assert!(input::recover(&paths[0], None).is_err());
            assert!(input::pending(&paths[0]).unwrap());
        }
        assert_eq!(std::fs::read(&vault).unwrap(), vault_before);
        assert_eq!(
            Proposals::at(paths[0].clone()).snapshot().unwrap().cells,
            before
        );
        transfer_intake_history(&paths[1], &paths[0], application, &parent);
        assert!(imported.snapshot().unwrap().history_complete());
        assert!(
            Proposals::at(paths[0].clone())
                .snapshot()
                .unwrap()
                .history_complete()
        );
        if during_pending {
            assert!(input::recover(&paths[0], None).unwrap());
        } else {
            input::set(&paths[0], &selector, b"synthetic-key").unwrap();
        }
        assert!(!input::pending(&paths[0]).unwrap());
        let after = Proposals::at(paths[0].clone()).snapshot().unwrap();
        let attached: Vec<_> = after
            .cells
            .values()
            .flat_map(|cell| &cell.candidates)
            .filter(|candidate| {
                candidate.value["source"]["import"]["value"]
                    .as_str()
                    .is_some_and(|link| link.ends_with(&selector))
            })
            .collect();
        assert_eq!(attached.len(), 1);
        assert!(attached[0].value["secret_ref"].is_object());
    }
}

#[test]
fn automatic_native_wait_requests_then_wakes_without_a_second_launch() {
    let (_temp, approved, config) = fixture(Provider::Google, true);
    set_policy_mode(&approved.paths.home, [7; 32], Vec::new(), true).unwrap();
    let now = Instant::now();
    let mut pauses = 0;
    let result = wait_selected(&approved.paths.home, Provider::Google, &config, || {
        pauses += 1;
        assert_eq!(pauses, 1);
        let admin = client::admin_cached(&approved.paths).unwrap();
        assert_eq!(
            allocation::step_at(&approved.paths, &admin, &config, deadline(), now).unwrap(),
            0
        );
        assert_eq!(
            allocation::step_at(
                &approved.paths,
                &admin,
                &config,
                deadline(),
                now + Duration::from_secs(60)
            )
            .unwrap(),
            1
        );
    })
    .unwrap();
    assert_eq!(result.account, approved.account);
    assert_eq!(pauses, 1);
    let view =
        aterm_db::accounts::native_requests::Requests::at_until(approved.paths.clone(), deadline())
            .view()
            .unwrap();
    assert_eq!(view.requests.len(), 1);
}

#[test]
fn enabling_automatic_allocation_preserves_the_same_subjects_valid_warm_slots() {
    let (_temp, approved, _) = fixture(Provider::Google, true);
    set_policy(&approved.paths.home, [7; 32], vec!["m1".into()]).unwrap();
    set_policy_mode(&approved.paths.home, [7; 32], Vec::new(), true).unwrap();
    let (policy, _) = policy(&approved.paths, &approved.expected)
        .unwrap()
        .unwrap();
    assert_eq!(
        policy.automatic(Provider::Google).unwrap().unwrap().warm,
        vec!["m1"]
    );
}

#[test]
fn automatic_allocation_does_not_take_an_explicit_fixed_slot() {
    let (_temp, approved, config) = fixture(Provider::Google, true);
    set_policy_mode(&approved.paths.home, [7; 32], Vec::new(), true).unwrap();
    let (mut policy, _) = policy(&approved.paths, &approved.expected)
        .unwrap()
        .unwrap();
    let subject = policy.automatic(Provider::Google).unwrap().unwrap().subject;
    policy
        .set(Assignment {
            subject: Subject {
                account: [19; 32],
                revision: [20; 32],
                scope: Scope::Preparation,
                provider: Provider::Google,
            },
            warm: vec!["m1".into()],
            automatic: None,
        })
        .unwrap();
    let admin = client::admin_cached(&approved.paths).unwrap();
    client::admin_set_entry_until(
        &approved.paths,
        &admin,
        ENTRY,
        admin.native_entry_digest(ENTRY).unwrap(),
        policy.text().as_bytes(),
        deadline(),
    )
    .unwrap();
    allocation::request(&approved.paths, &policy, subject, deadline()).unwrap();
    let admin = client::admin_cached(&approved.paths).unwrap();
    let now = Instant::now();
    assert_eq!(
        allocation::step_at(&approved.paths, &admin, &config, deadline(), now).unwrap(),
        0
    );
    assert_eq!(
        allocation::step_at(
            &approved.paths,
            &admin,
            &config,
            deadline(),
            now + Duration::from_secs(60)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        super::policy(&approved.paths, &approved.expected)
            .unwrap()
            .unwrap()
            .0,
        policy
    );
}

/// Sweeps of a Meta automatic allocation at `now` plus a number of seconds, and a fresh grant
/// with a request under it (the last grant's warm slot set aside first).
fn allocation_sweeps(approved: &Approved, config: &Path) -> (impl Fn(u64) -> usize, impl Fn()) {
    let paths = approved.paths.clone();
    let expected = approved.expected.clone();
    let config = config.to_owned();
    let now = Instant::now();
    let home = paths.home.clone();
    let sweep = {
        let paths = paths.clone();
        move |at: u64| {
            let admin = client::admin_cached(&paths).unwrap();
            allocation::step_at(
                &paths,
                &admin,
                &config,
                deadline(),
                now + Duration::from_secs(at),
            )
            .unwrap()
        }
    };
    let grant_and_ask = move || {
        set_policy(&home, [7; 32], Vec::new()).unwrap();
        set_policy_mode(&home, [7; 32], Vec::new(), true).unwrap();
        let (policy, _) = policy(&paths, &expected).unwrap().unwrap();
        let subject = policy.automatic(Provider::Meta).unwrap().unwrap().subject;
        allocation::request(&paths, &policy, subject, deadline()).unwrap();
    };
    (sweep, grant_and_ask)
}

/// A ready allocation sweep with nothing to change leaves the cooldown unspent, so a request
/// that arrives after it is published at the next sweep. Control: the first sweep of a grant
/// waits the cooldown out.
#[test]
fn an_idle_ready_allocation_sweep_leaves_the_cooldown_unspent() {
    let (_temp, approved, config) = fixture(Provider::Meta, true);
    let (sweep, grant_and_ask) = allocation_sweeps(&approved, &config);
    grant_and_ask();
    assert_eq!(sweep(0), 0, "the first sight starts the cooldown");
    assert_eq!(sweep(60), 1);
    assert_eq!(sweep(120), 0, "ready, with nothing to change");
    grant_and_ask();
    assert_eq!(sweep(121), 1, "the idle sweep left the cooldown unspent");
}

/// The sweep that publishes starts the cooldown: a request after it waits the cooldown out.
#[test]
fn an_allocation_publication_starts_the_cooldown() {
    let (_temp, approved, config) = fixture(Provider::Meta, true);
    let (sweep, grant_and_ask) = allocation_sweeps(&approved, &config);
    grant_and_ask();
    assert_eq!(sweep(0), 0);
    assert_eq!(sweep(60), 1);
    grant_and_ask();
    assert_eq!(sweep(61), 0, "the publication at 60 started the cooldown");
    assert_eq!(sweep(120), 1);
}

#[test]
fn automatic_wait_survives_a_temporarily_busy_vault_before_the_grant() {
    let (_temp, approved, config) = fixture(Provider::Google, true);
    set_policy_mode(&approved.paths.home, [7; 32], Vec::new(), true).unwrap();
    let mut held = None;
    let mut pauses = 0;
    let now = Instant::now();
    let result = wait_selected(&approved.paths.home, Provider::Google, &config, || {
        pauses += 1;
        assert!(pauses <= 2);
        if pauses == 1 {
            let (file, _) =
                aterm_uds::private_file::open_private(&approved.paths.dir, ".lock").unwrap();
            aterm_uds::file_lock::lock_exclusive_until(&file, deadline(), "test lock busy")
                .unwrap();
            held = Some(file);
        } else {
            drop(held.take());
            let admin = client::admin_cached(&approved.paths).unwrap();
            allocation::step_at(&approved.paths, &admin, &config, deadline(), now).unwrap();
            assert_eq!(
                allocation::step_at(
                    &approved.paths,
                    &admin,
                    &config,
                    deadline(),
                    now + Duration::from_secs(60)
                )
                .unwrap(),
                1
            );
        }
    })
    .unwrap();
    assert_eq!(result.account, approved.account);
    assert_eq!(pauses, 2);
}

#[test]
fn native_wait_refuses_initial_bad_config_and_a_positive_authority_change() {
    let (_temp, approved, config) = fixture(Provider::Google, true);
    set_policy_mode(&approved.paths.home, [7; 32], Vec::new(), true).unwrap();
    std::fs::write(&config, "[harness\n").unwrap();
    assert!(
        wait_selected(&approved.paths.home, Provider::Google, &config, || panic!(
            "invalid initial configuration must not start waiting"
        ))
        .is_err()
    );
    std::fs::write(&config, "").unwrap();
    let mut pauses = 0;
    assert!(
        wait_selected(&approved.paths.home, Provider::Google, &config, || {
            pauses += 1;
            assert_eq!(pauses, 1);
            std::fs::write(
                approved.paths.dir.join("source.toml"),
                format!(
                    "path = {}\nname = \"other\"\nhost = \"m1\"\n",
                    json!(_temp.path().join("other.vault").to_str().unwrap())
                ),
            )
            .unwrap();
        })
        .is_err()
    );
    assert_eq!(pauses, 1);
}

#[test]
fn reissued_automatic_grant_does_not_reuse_old_request_and_off_cancels_wait() {
    let (_temp, approved, config) = fixture(Provider::Meta, true);
    set_policy_mode(&approved.paths.home, [7; 32], Vec::new(), true).unwrap();
    let (before, _) = policy(&approved.paths, &approved.expected)
        .unwrap()
        .unwrap();
    let subject = before.automatic(Provider::Meta).unwrap().unwrap().subject;
    allocation::request(&approved.paths, &before, subject, deadline()).unwrap();
    set_policy_mode(&approved.paths.home, [7; 32], Vec::new(), true).unwrap();
    let (after, _) = policy(&approved.paths, &approved.expected)
        .unwrap()
        .unwrap();
    assert_ne!(
        before
            .automatic(Provider::Meta)
            .unwrap()
            .unwrap()
            .automatic
            .as_ref()
            .unwrap()
            .grant,
        after
            .automatic(Provider::Meta)
            .unwrap()
            .unwrap()
            .automatic
            .as_ref()
            .unwrap()
            .grant
    );
    let admin = client::admin_cached(&approved.paths).unwrap();
    let now = Instant::now();
    assert_eq!(
        allocation::step_at(&approved.paths, &admin, &config, deadline(), now).unwrap(),
        0
    );
    assert_eq!(
        allocation::step_at(
            &approved.paths,
            &admin,
            &config,
            deadline(),
            now + Duration::from_secs(60)
        )
        .unwrap(),
        0
    );
    assert!(
        wait_selected(&approved.paths.home, Provider::Meta, &config, || {
            std::fs::write(&config, "[harness]\naccounts='off'\n").unwrap()
        })
        .is_err()
    );
}

#[test]
fn native_intake_resumes_signed_sealed_input_before_its_first_publication() {
    let (_temp, approved, _config) = fixture(Provider::Google, false);
    assert!(
        input::set_after_pending(&approved.paths, "native", b"crash-retained-key", || Err(
            "simulated stop".into()
        ))
        .is_err()
    );
    let pending = std::fs::read(approved.paths.dir.join("native-key-intake.age")).unwrap();
    assert!(
        !pending
            .windows(b"crash-retained-key".len())
            .any(|w| w == b"crash-retained-key")
    );
    assert!(input::recover(&approved.paths, None).unwrap());
    assert!(!input::pending(&approved.paths).unwrap());
    let state = Activations::at(approved.paths.clone())
        .state(approved.account)
        .unwrap();
    assert_eq!(
        state,
        aterm_db::accounts::activations::State::Ready(approved.decision)
    );
    assert!(observation(&approved, deadline()).is_err());
}

#[test]
fn native_intake_resume_uses_published_entry_despite_an_unpublished_draft() {
    let (_temp, approved, config) = fixture(Provider::Google, false);
    assert!(
        input::set_after_pending(&approved.paths, "native", b"retained-key", || Err(
            "simulated stop".into()
        ))
        .is_err()
    );
    let scope = Proposals::at(approved.paths.clone())
        .snapshot()
        .unwrap()
        .scope;
    let name = input::entry(&scope, "native", Provider::Google);
    let mut admin = client::admin_cached(&approved.paths).unwrap();
    admin
        .doc
        .set_entry(&Audience::Everyone, &name, b"unpublished")
        .unwrap();
    assert!(input::recover_root(&approved.paths, &admin, &config, deadline()).unwrap());
    let expected = client::config(&approved.paths).unwrap().unwrap();
    let entry = client::read_verified_entry(&approved.paths, &expected, &name)
        .unwrap()
        .unwrap();
    assert!(Inputs::decode(entry.value().as_slice(), Provider::Google).is_ok());
    assert_ne!(entry.value().as_slice(), b"unpublished");
    assert_eq!(
        admin
            .doc
            .entry(&Audience::Everyone, &name)
            .unwrap()
            .value
            .as_slice(),
        b"unpublished"
    );
}

#[test]
fn native_intake_refuses_altered_or_foreign_root_capsules() {
    let (_temp, approved, _config) = fixture(Provider::Google, false);
    assert!(
        input::set_after_pending(&approved.paths, "native", b"retained-key", || Err(
            "simulated stop".into()
        ))
        .is_err()
    );
    let original = std::fs::read(approved.paths.dir.join("native-key-intake.age")).unwrap();
    let mut changed = original.clone();
    let end = changed.len() - 1;
    changed[end] ^= 1;
    fsx::write_atomic(
        &approved.paths.dir.join("native-key-intake.age"),
        &changed,
        0o600,
    )
    .unwrap();
    assert!(input::recover(&approved.paths, None).is_err());
    let (_other, other, _) = fixture(Provider::Google, false);
    fsx::write_atomic(
        &other.paths.dir.join("native-key-intake.age"),
        &original,
        0o600,
    )
    .unwrap();
    assert!(input::recover(&other.paths, None).is_err());
    let identity = client::read_machine_identity(&other.paths)
        .unwrap()
        .unwrap();
    let forged =
        aterm_secrets::age::encrypt(&[identity.recipient()], b"not authorized by the root")
            .unwrap();
    fsx::write_atomic(
        &other.paths.dir.join("native-key-intake.age"),
        &forged,
        0o600,
    )
    .unwrap();
    assert!(input::recover(&other.paths, None).is_err());
}

#[test]
fn native_intake_capsule_checks_exact_root_device_and_signature() {
    let (_temp, approved, _) = fixture(Provider::Google, false);
    let admin = client::admin_cached(&approved.paths).unwrap();
    let bootstrap = Bootstrap::required(&approved.paths).unwrap();
    let request = bootstrap.request().unwrap();
    let identity = client::read_machine_identity(&approved.paths)
        .unwrap()
        .unwrap();
    let sealed = admin
        .seal_account_input(request.binding(), b"synthetic input")
        .unwrap();
    let root = bootstrap.trust().authority;
    assert_eq!(
        client::open_account_input(&root, request.binding(), &identity, &sealed)
            .unwrap()
            .as_slice(),
        b"synthetic input"
    );
    let mut wrong = root;
    wrong[0] ^= 1;
    assert!(client::open_account_input(&wrong, request.binding(), &identity, &sealed).is_err());
    let (_other, other, _) = fixture(Provider::Google, false);
    let other_boot = Bootstrap::required(&other.paths).unwrap();
    let other_request = other_boot.request().unwrap();
    assert!(
        client::open_account_input(&root, other_request.binding(), &identity, &sealed).is_err()
    );
    let mut changed = sealed.clone();
    let i = changed.len() - 1;
    changed[i] ^= 1;
    assert!(client::open_account_input(&root, request.binding(), &identity, &changed).is_err());
}

#[test]
fn native_intake_automatic_resume_respects_off_and_retains_exact_input() {
    let (_temp, approved, config) = fixture(Provider::Google, false);
    assert!(
        input::set_after_pending(&approved.paths, "native", b"retained-key", || Err(
            "simulated stop".into()
        ))
        .is_err()
    );
    let before = std::fs::read(approved.paths.dir.join("native-key-intake.age")).unwrap();
    let admin = client::admin_cached(&approved.paths).unwrap();
    std::fs::write(&config, "[harness]\naccounts='off'\n").unwrap();
    assert!(input::recover_root(&approved.paths, &admin, &config, deadline()).is_err());
    assert_eq!(
        std::fs::read(approved.paths.dir.join("native-key-intake.age")).unwrap(),
        before
    );
    std::fs::write(&config, "").unwrap();
    assert!(input::recover_root(&approved.paths, &admin, &config, deadline()).unwrap());
}

#[test]
fn native_intake_preserves_a_proposal_changed_after_the_input_was_sealed() {
    let (_temp, approved, _config) = fixture(Provider::Meta, false);
    let proposals = Proposals::at(approved.paths.clone());
    let mut changed = proposals.snapshot().unwrap().cells[&("accounts".into(), "native".into())]
        .candidates[0]
        .value
        .clone();
    changed
        .as_object_mut()
        .unwrap()
        .insert("plan".into(), json!("later-plan"));
    assert!(
        input::set_after_pending(&approved.paths, "native", b"retained-key", || {
            proposals
                .write(
                    [99; 16],
                    vec![Change {
                        collection: "accounts".into(),
                        key: "native".into(),
                        value: Some(changed.clone()),
                    }],
                )
                .unwrap();
            Err("simulated stop".into())
        })
        .is_err()
    );
    assert!(input::recover(&approved.paths, None).is_err());
    assert_eq!(
        proposals.snapshot().unwrap().cells[&("accounts".into(), "native".into())].candidates[0]
            .value,
        changed
    );
}

#[test]
fn native_reply_races_cannot_publish_access_for_revocation_rotation_or_local_off() {
    for change in 0..3 {
        let (_temp, approved, config) = fixture(Provider::Google, false);
        let mut server = Server {
            calls: 0,
            after: |call| {
                if call != 2 {
                    return;
                }
                match change {
                    0 => {
                        let db = Activations::at(approved.paths.clone());
                        let admin = client::admin_open(&approved.paths).unwrap();
                        let stop = db
                            .stop_request(approved.account, vec![approved.decision])
                            .unwrap();
                        db.issue(&admin, [3; 16], stop).unwrap();
                    }
                    1 => {
                        let admin = client::admin_open(&approved.paths).unwrap();
                        let mut doc = admin.doc.clone();
                        doc.set_entry(&Audience::Everyone,"native-input",br#"{"schema":1,"kind":"api-key","provider":"google","key":"replacement"}"#).unwrap();
                        client::admin_publish(&approved.paths, &admin, &doc).unwrap();
                    }
                    _ => std::fs::write(&config, "[harness]\naccounts = 'off'\n").unwrap(),
                }
            },
        };
        assert!(prepare_one(&approved, &config, &mut server, deadline()).is_err());
        assert_eq!(
            records(&approved.paths).unwrap()[0]["state"].as_str(),
            Some("pending")
        );
    }
}

#[test]
fn native_selection_stays_held_after_assignment_removal_but_off_is_local() {
    let (_temp, approved, config) = fixture(Provider::Google, false);
    assert!(!selected_launch(&approved.paths, &config, Provider::Google, deadline()).unwrap());
    set_policy(&approved.paths.home, approved.account, vec!["m1".into()]).unwrap();
    assert!(selected_launch(&approved.paths, &config, Provider::Google, deadline()).unwrap());
    assert!(selected_launch(&approved.paths, &config, Provider::Google, deadline()).unwrap());
    set_policy(&approved.paths.home, approved.account, vec![]).unwrap();
    assert!(selected_launch(&approved.paths, &config, Provider::Google, deadline()).unwrap());
    assert!(selected(&approved.paths.home, Provider::Google, deadline()).is_err());
    std::fs::write(&config, "[harness]\naccounts = 'off'\n").unwrap();
    assert!(!selected_launch(&approved.paths, &config, Provider::Google, deadline()).unwrap());
    let (_other, other, other_config) = fixture(Provider::Google, false);
    std::fs::copy(
        approved.paths.dir.join(SELECTION),
        other.paths.dir.join(SELECTION),
    )
    .unwrap();
    assert!(!selected_launch(&other.paths, &other_config, Provider::Google, deadline()).unwrap());
}

#[test]
fn native_intent_policy_follows_recovery_but_not_changed_intent_or_credentials() {
    let (_temp, approved, config) = fixture(Provider::Meta, true);
    set_policy(&approved.paths.home, approved.account, vec!["m1".into()]).unwrap();
    let observed = observation(&approved, deadline()).unwrap();
    permits(&approved, &observed, &config, deadline()).unwrap();
    let admin = client::admin_open(&approved.paths).unwrap();
    let db = Activations::at(approved.paths.clone());
    let requester = Bootstrap::required(&approved.paths)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .clone();
    let (next, _) = db
        .reissue_intent_preparation(
            &admin,
            approved.account,
            approved.decision,
            &requester,
            [3; 16],
        )
        .unwrap();
    assert!(permits(&approved, &observed, &config, deadline()).is_err());
    let current = Approved {
        paths: approved.paths.clone(),
        expected: approved.expected.clone(),
        account: approved.account,
        decision: next.id(),
    };
    let next = observation(&current, deadline()).unwrap();
    permits(&current, &next, &config, deadline()).unwrap();
    let stop = db
        .stop_request(observed.subject.account, vec![observed.subject.revision])
        .unwrap();
    db.issue(&admin, [4; 16], stop).unwrap();
    assert!(permits(&current, &next, &config, deadline()).is_err());
}

#[test]
fn native_child_uses_private_profiles_and_no_ambient_route_or_secret() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let original = temp.path().join("person-profile");
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("config"), "keep").unwrap();
    for provider in [Provider::Google, Provider::Meta] {
        let profile = temp.path().join(provider.name());
        let vars = [
            ("PATH", "/usr/bin"),
            ("HOME", original.to_str().unwrap()),
            ("AGY_GATEWAY_URL", "https://foreign.invalid"),
            ("MUSE_CUSTOM_HEADERS", "secret"),
            ("TBH_CREDENTIAL_BACKEND", "foreign"),
            ("GOOGLE_API_KEY", "other"),
            ("GEMINI_API_KEY", "other"),
            ("META_API_KEY", "other"),
        ];
        let command = child_command(
            provider,
            Path::new("/not-executed"),
            &profile,
            &[],
            vars.into_iter().map(|(k, v)| (k.into(), v.into())),
        )
        .unwrap();
        let env: BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(env[std::ffi::OsStr::new("HOME")], Some(profile.as_os_str()));
        assert!(!env.contains_key(std::ffi::OsStr::new(provider.key_env())));
        assert!(!format!("{command:?}").contains("foreign.invalid"));
        assert!(!format!("{command:?}").contains("other"));
    }
    assert_eq!(std::fs::read(original.join("config")).unwrap(), b"keep");
    assert!(
        child_command(
            Provider::Google,
            Path::new("/not-executed"),
            &original,
            &[],
            []
        )
        .is_err()
    );
    assert_eq!(std::fs::read(original.join("config")).unwrap(), b"keep");
}

#[test]
fn native_launch_without_shared_selection_does_not_require_commons() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(temp.path());
    client::test_create_unbound(
        &paths,
        temp.path().join("legacy.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let config = temp.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    assert!(Bootstrap::required_until(&paths, deadline()).is_err());
    for provider in [Provider::Google, Provider::Meta] {
        assert!(!selected_launch(&paths, &config, provider, deadline()).unwrap());
    }
    fsx::write_atomic(
        &paths.dir.join("source.toml"),
        b"url = 'https://example.invalid/personal.vault'\nname = 'personal'\nhost = 'm1'\n",
        0o600,
    )
    .unwrap();
    for provider in [Provider::Google, Provider::Meta] {
        assert!(!selected_launch(&paths, &config, provider, deadline()).unwrap());
    }
    assert!(!paths.dir.join(SELECTION).exists());
}

/// A piped native key of at most 16 KiB is taken with its one LF or CR LF dropped; a longer
/// input is refused whole. The row the old two-byte margin cut: a full key, its CR LF, and more
/// that was never read.
#[test]
fn a_piped_native_key_past_its_bound_is_refused_not_cut() {
    let key = vec![b'k'; 16 * 1024];
    let piped = |tail: &[u8]| {
        let mut bytes = key.clone();
        bytes.extend_from_slice(tail);
        input::read_piped_key(bytes.as_slice()).map(|secret| secret.expose().to_vec())
    };
    for ending in [&b""[..], b"\n", b"\r\n"] {
        assert_eq!(piped(ending).unwrap(), key, "{ending:?}");
    }
    for longer in [&b"\r\nx"[..], b"\nx", b"k", b"\n\n"] {
        assert!(piped(longer).is_err(), "{longer:?}");
    }
}

/// The Gemini CLI is handed its profile folder byte for byte: a name with `=` in it through the
/// launch, and one that is not UTF-8, which `display()` would have turned into another folder.
#[test]
fn the_gemini_profile_folder_is_handed_over_byte_for_byte() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let profile = temp.path().join("google-=dir");
    let command = child_command(
        Provider::Google,
        Path::new("/not-executed"),
        &profile,
        &[],
        [],
    )
    .unwrap();
    let mut want = OsString::from("--gemini_dir=");
    want.push(profile.as_os_str());
    assert_eq!(command.get_args().collect::<Vec<_>>(), [want.as_os_str()]);
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let foreign = Path::new(std::ffi::OsStr::from_bytes(b"/profiles/google-\xff"));
    assert_eq!(
        gemini_dir(foreign).into_vec(),
        b"--gemini_dir=/profiles/google-\xff"
    );
}

#[test]
fn native_child_pins_provider_without_changing_the_exec_verb_order() {
    let temp = aterm_tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec![
            "exec".into(),
            "--prompt-file".into(),
            "/private/prompt".into(),
        ],
    ] {
        let profile = temp
            .path()
            .join(if args.is_empty() { "tui" } else { "exec" });
        let command = child_command(
            Provider::Meta,
            Path::new("/not-executed"),
            &profile,
            &args,
            [],
        )
        .unwrap();
        let words = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        let prefix = if args.is_empty() {
            0
        } else {
            assert_eq!(words[0], "exec");
            1
        };
        assert_eq!(
            &words[prefix..prefix + 4],
            &["--provider", "meta", "--base-url", "https://api.meta.ai/v1"]
        );
    }
    let profile = temp.path().join("google");
    let command = child_command(
        Provider::Google,
        Path::new("/not-executed"),
        &profile,
        &[],
        [],
    )
    .unwrap();
    assert!(
        command
            .get_envs()
            .any(|(name, value)| name == "GOOGLE_GEMINI_BASE_URL"
                && value
                    == Some(std::ffi::OsStr::new(
                        "https://generativelanguage.googleapis.com"
                    )))
    );
    let settings = profile.join("antigravity-cli/settings.json");
    let value: Value = aterm_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(value, json!({"modelProvider":"gemini"}));
    assert!(!profile.join("antigravity-cli/onboarding.json").exists());
    std::fs::write(
        &settings,
        br#"{"modelProvider":"gemini","endpoint":"https://elsewhere.invalid"}"#,
    )
    .unwrap();
    assert!(
        child_command(
            Provider::Google,
            Path::new("/not-executed"),
            &profile,
            &[],
            []
        )
        .is_err()
    );
}
