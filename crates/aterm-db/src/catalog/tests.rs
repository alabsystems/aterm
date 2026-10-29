// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    control::{Endpoint, Repair, Side},
    table::tests::Lab,
};
use aterm_net::lan::{Connection, Scope as LinkScope};
use aterm_secrets::{commons::roster::Role, fsx};
use std::{net::TcpListener, thread, time::Duration};

struct Pair {
    lab: Lab,
    configs: [PathBuf; 2],
    trust: Trust,
}
impl Pair {
    fn new() -> Self {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let configs = [
            lab.paths[0].home.join("config.toml"),
            lab.paths[1].home.join("config.toml"),
        ];
        let trust = lab.bootstrap(0).trust();
        Self {
            lab,
            configs,
            trust,
        }
    }
    fn directory(&self, n: usize) -> Directory {
        Directory::new(&self.lab.paths[n], self.trust)
    }
    fn endpoint(&self, n: usize) -> Endpoint {
        Endpoint::from_bootstrap(&self.lab.bootstrap(n))
            .unwrap()
            .unwrap()
            .with_commons(self.lab.paths[n].clone(), self.configs[n].clone())
            .unwrap()
    }
    fn round(&self, repairs: &mut [Repair; 2]) {
        let endpoints = [self.endpoint(0), self.endpoint(1)];
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let [ra, rb] = repairs;
        thread::scope(|threads| {
            let server = threads.spawn(|| {
                endpoints[1].exchange(
                    Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                    Side::Responder,
                    rb,
                )
            });
            endpoints[0]
                .exchange(
                    Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60))
                        .unwrap(),
                    Side::Initiator,
                    ra,
                )
                .unwrap();
            server.join().unwrap().unwrap();
        });
        assert!(
            ra.application().is_some() && rb.application().is_some(),
            "{ra:?}\n{rb:?}"
        );
    }
    fn app(&self, n: usize, version: u32) -> Scope {
        let bootstrap = self.lab.bootstrap(n);
        self.directory(n)
            .open_application(
                &Session::open(&bootstrap).unwrap(),
                "accounts",
                "proposals",
                version,
            )
            .unwrap()
    }
    fn count(&self, n: usize, scope: Scope) -> usize {
        let bootstrap = self.lab.bootstrap(n);
        let session = Session::open(&bootstrap).unwrap();
        Store::open(&session, &self.directory(n).path(scope).unwrap(), scope)
            .unwrap()
            .inventory(&session)
            .unwrap()
            .count()
    }
}

#[test]
fn default_tls_discovers_offline_creation_then_repairs_without_provisioning() {
    let p = Pair::new();
    let scope = p.app(0, 1);
    let bootstrap = p.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::open(&session, &p.directory(0).path(scope).unwrap(), scope).unwrap();
    store
        .commit_catalog(&session, b"inert account proposal")
        .unwrap();
    drop(store);
    drop(session);
    assert!(!p.directory(1).path(scope).unwrap().exists());
    let mut repair = [Repair::default(), Repair::default()];
    for _ in 0..4 {
        p.round(&mut repair);
    }
    assert_eq!(p.count(1, scope), 1);
    let bootstrap = p.lab.bootstrap(1);
    let snapshot = p
        .directory(1)
        .snapshot(&Session::open(&bootstrap).unwrap())
        .unwrap();
    assert_eq!(snapshot.entries[&scope.digest()].state, State::Subscribed);
    assert_eq!(
        snapshot.entries[&scope.digest()].declarations[0].actor,
        p.lab.bootstrap(0).request().unwrap().binding().actor()
    );
}

#[test]
fn independent_offline_application_opens_coalesce_and_keep_both_authors() {
    let p = Pair::new();
    let a = p.app(0, 1);
    let b = p.app(1, 1);
    assert_eq!(a, b);
    let mut repair = [Repair::default(), Repair::default()];
    for _ in 0..4 {
        p.round(&mut repair);
    }
    for n in 0..2 {
        let bootstrap = p.lab.bootstrap(n);
        let session = Session::open(&bootstrap).unwrap();
        let snapshot = p.directory(n).snapshot(&session).unwrap();
        let entry = &snapshot.entries[&a.digest()];
        assert_eq!(entry.state, State::Subscribed);
        assert_eq!(entry.declarations.len(), 2);
        p.directory(n)
            .open_application(&session, "accounts", "proposals", 1)
            .unwrap();
        assert_eq!(
            p.directory(n).snapshot(&session).unwrap().entries[&a.digest()]
                .declarations
                .len(),
            2
        );
    }
}

#[test]
fn incompatible_offline_declarations_block_automation_without_replacing_identity() {
    let p = Pair::new();
    let a = p.app(0, 1);
    let b = p.app(1, 2);
    assert_eq!(a, b);
    let mut repair = [Repair::default(), Repair::default()];
    for _ in 0..4 {
        p.round(&mut repair);
    }
    for n in 0..2 {
        let bootstrap = p.lab.bootstrap(n);
        let session = Session::open(&bootstrap).unwrap();
        let dir = p.directory(n);
        assert_eq!(
            dir.snapshot(&session).unwrap().entries[&a.digest()].state,
            State::Conflict
        );
        assert!(
            !dir.subscriptions(&session, &Policy::default())
                .unwrap()
                .contains_key(&a.digest())
        );
        assert!(
            dir.open_application(&session, "accounts", "proposals", 1)
                .is_err()
        );
    }
}

#[test]
fn excluded_tables_are_not_enrolled_but_other_tables_continue() {
    let p = Pair::new();
    let scope = p.app(0, 1);
    fsx::write_atomic(
        &p.configs[1],
        format!(
            "[commons]\nexclude_tables=['{}']\n",
            aterm_codec::hex::encode_lower(&scope.digest())
        )
        .as_bytes(),
        0o600,
    )
    .unwrap();
    let mut repair = [Repair::default(), Repair::default()];
    for _ in 0..3 {
        p.round(&mut repair);
    }
    assert!(!p.directory(1).path(scope).unwrap().exists());
    let bootstrap = p.lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    assert_eq!(p.directory(1).snapshot(&session).unwrap().entries.len(), 1);
    drop(session);
    fsx::write_atomic(&p.configs[1], b"[commons]\n", 0o600).unwrap();
    for _ in 0..3 {
        p.round(&mut repair);
    }
    assert!(p.directory(1).path(scope).unwrap().exists());
}

#[test]
fn damaged_automatic_table_is_not_reset_and_catalog_remains_available() {
    let p = Pair::new();
    let scope = p.app(0, 1);
    let path = p.directory(0).path(scope).unwrap();
    std::fs::remove_file(path.join("table.checkpoint")).unwrap();
    let bootstrap = p.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let subscriptions = p
        .directory(0)
        .subscriptions(&session, &Policy::default())
        .unwrap();
    assert!(subscriptions.contains_key(&scope.digest()));
    assert!(p.directory(0).enroll(&session, scope).is_err());
    assert!(subscriptions.contains_key(&p.directory(0).catalog_scope().digest()));
    assert!(!path.join("table.checkpoint").exists());
}

#[test]
fn malformed_descriptor_is_inert_and_cannot_choose_a_local_path() {
    let p = Pair::new();
    let bootstrap = p.lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let dir = p.directory(0);
    let descriptor =
        Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1).unwrap();
    assert_eq!(
        Descriptor::decode(bootstrap.trust(), &descriptor.encode().unwrap()).unwrap(),
        descriptor
    );
    let mut value: aterm_json::Value =
        aterm_json::from_slice(&descriptor.encode().unwrap()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("path".into(), aterm_json::Value::from("../../elsewhere"));
    let mut catalog = dir.catalog(&session).unwrap();
    catalog
        .commit_catalog(&session, &aterm_json::to_vec(&value).unwrap())
        .unwrap();
    drop(catalog);
    let snapshot = dir.snapshot(&session).unwrap();
    assert!(snapshot.entries.is_empty());
    assert_eq!(snapshot.inert, 1);
    let mut wrong = descriptor.clone();
    wrong.scope = Scope::fresh(bootstrap.trust()).unwrap();
    assert!(wrong.encode().is_err());
    let mut foreign = descriptor.scope.trust();
    foreign.genesis[0] ^= 1;
    assert!(
        dir.path(Scope::new(foreign, *descriptor.scope.table(), 1).unwrap())
            .is_err()
    );
}

#[test]
fn subscription_policy_parses_fail_closed_and_preserves_default_on() {
    let home = aterm_tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    assert_eq!(Policy::read(&config).unwrap(), Policy::default());
    fsx::write_atomic(&config, b"[swarm]\nenabled = false\n", 0o600).unwrap();
    assert!(!Policy::read(&config).unwrap().enabled);
    fsx::write_atomic(
        &config,
        b"[swarm]\nenabled = false\n[commons]\nenabled = true\n",
        0o600,
    )
    .unwrap();
    assert!(
        !Policy::read(&config).unwrap().enabled,
        "[swarm] enabled = false overrides [commons] enabled = true"
    );
    for text in [
        "[commons]\nexclude_tables=1",
        "[commons]\nexclude_tables=['bad']",
        "[commons]\nenabled=1",
        "[commons]\nexclude_table=[]",
        "[swarm]\nenabled=1",
        "[swarm]\nother=true",
    ] {
        fsx::write_atomic(&config, text.as_bytes(), 0o600).unwrap();
        assert!(Policy::read(&config).is_err());
    }
    let id = "a".repeat(64);
    fsx::write_atomic(
        &config,
        format!("[commons]\nexclude_tables=['{id}','{id}']").as_bytes(),
        0o600,
    )
    .unwrap();
    assert!(Policy::read(&config).is_err());
}

#[test]
fn overflowing_creator_cannot_displace_others_or_hide_conflicts() {
    let p = Pair::new();
    let a = p.lab.bootstrap(0);
    let b = p.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let dir = p.directory(0);
    let mut catalog = dir.catalog(&sa).unwrap();
    let other = Descriptor::application(a.trust(), "other", "data", 1).unwrap();
    let wire = crate::table::tests::seal(
        &sb,
        dir.catalog_scope(),
        crate::crypto::Causal {
            sequence: 1,
            previous: None,
            parents: &[],
        },
        &other.encode().unwrap(),
    )
    .unwrap();
    catalog.ingest(&sa, &wire).unwrap();
    for n in 0..=PER_CREATOR {
        let d = Descriptor::application(a.trust(), "spam", &format!("table{n}"), 1).unwrap();
        catalog.commit_catalog(&sa, &d.encode().unwrap()).unwrap();
    }
    drop(catalog);
    let snapshot = dir.snapshot(&sa).unwrap();
    assert_eq!(
        snapshot.entries[&other.scope.digest()].state,
        State::Subscribed
    );
    assert_eq!(
        snapshot
            .entries
            .values()
            .filter(|e| e.state == State::CreatorLimit)
            .count(),
        1
    );
    let mut catalog = dir.catalog(&sa).unwrap();
    let mut conflict = other.clone();
    conflict.version = 2;
    catalog
        .commit_catalog(&sa, &conflict.encode().unwrap())
        .unwrap();
    drop(catalog);
    assert_eq!(
        dir.snapshot(&sa).unwrap().entries[&other.scope.digest()].state,
        State::Conflict
    );
    let source = dir.catalog(&sa).unwrap();
    let peer = p.directory(1);
    let mut target = peer.catalog(&sb).unwrap();
    for id in source.inventory(&sa).unwrap() {
        target
            .ingest(&sb, source.wire(&sa, id).unwrap().unwrap())
            .unwrap();
    }
    drop(target);
    let rescued =
        Descriptor::application(a.trust(), "spam", &format!("table{PER_CREATOR}"), 1).unwrap();
    peer.publish(&sb, &rescued).unwrap();
    let snapshot = peer.snapshot(&sb).unwrap();
    assert_eq!(
        snapshot.entries[&rescued.scope.digest()].state,
        State::Subscribed
    );
    assert_eq!(
        snapshot.entries[&other.scope.digest()].state,
        State::Conflict
    );
}

#[test]
fn catalog_admission_caps_payloads_and_counts_all_retained_author_history() {
    let p = Pair::new();
    let a = p.lab.bootstrap(0);
    let b = p.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let da = p.directory(0);
    let db = p.directory(1);
    let mut source = da.catalog(&sa).unwrap();
    let mut target = db.catalog(&sb).unwrap();
    let mut first = None;
    for _ in 0..RECORDS_PER_CREATOR {
        let receipt = source.commit_catalog(&sa, b"{}").unwrap();
        let wire = source.wire(&sa, &receipt.id).unwrap().unwrap();
        let received = target.ingest(&sb, wire).unwrap();
        if first.is_none() {
            first = Some((wire.to_vec(), received));
        }
    }
    assert!(source.commit_catalog(&sa, b"{}").is_err());
    let last = source.catalog(&sa).unwrap().last().unwrap().header().id;
    let beyond = crate::table::tests::seal(
        &sa,
        da.catalog_scope(),
        crate::crypto::Causal {
            sequence: RECORDS_PER_CREATOR as u64 + 1,
            previous: Some(last),
            parents: &[last],
        },
        b"{}",
    )
    .unwrap();
    assert!(source.ingest(&sa, &beyond).is_err());
    assert!(target.ingest(&sb, &beyond).is_err());
    let (wire, receipt) = first.unwrap();
    assert_eq!(target.ingest(&sb, &wire).unwrap(), receipt);
    let too_large = crate::table::tests::seal(
        &sb,
        db.catalog_scope(),
        crate::crypto::Causal {
            sequence: 1,
            previous: None,
            parents: &[],
        },
        &vec![0; 4097],
    )
    .unwrap();
    assert!(target.ingest(&sb, &too_large).is_err());
    assert_eq!(
        target.retained_by(&sb, sa.actor()).unwrap(),
        RECORDS_PER_CREATOR
    );
    drop(target);
    db.open_application(&sb, "honest", "data", 1).unwrap();
    assert_eq!(db.snapshot(&sb).unwrap().entries.len(), 1);
}

#[test]
fn retained_directory_limit_survives_changing_subscription_selection() {
    let p = Pair::new();
    let a = p.lab.bootstrap(0);
    let sa = Session::open(&a).unwrap();
    let dir = p.directory(0);
    let mut descriptors = [
        Descriptor::fresh(a.trust(), "existing", "data", 1).unwrap(),
        Descriptor::fresh(a.trust(), "later", "data", 1).unwrap(),
    ];
    descriptors.sort_by_key(|descriptor| descriptor.scope.digest());
    let [later, existing] = descriptors;
    let mut catalog = dir.catalog(&sa).unwrap();
    catalog
        .commit_catalog(&sa, &existing.encode().unwrap())
        .unwrap();
    drop(catalog);
    for _ in 1..MAX_TABLES {
        let scope = Scope::fresh(a.trust()).unwrap();
        Store::join(&sa, &dir.path(scope).unwrap(), scope).unwrap();
    }
    assert!(
        dir.open_application(&sa, "accounts", "proposals", 1)
            .is_err()
    );
    let selected = dir.subscriptions(&sa, &Policy::default()).unwrap();
    assert!(selected.contains_key(&existing.scope.digest()));
    // Selection reserves a directory, but only data repair enrolls a writer.
    assert!(Store::open(&sa, &dir.path(existing.scope).unwrap(), existing.scope).is_err());
    assert_eq!(
        std::fs::read_dir(&dir.root).unwrap().count(),
        MAX_TABLES + 1
    );
    let mut catalog = dir.catalog(&sa).unwrap();
    catalog
        .commit_catalog(&sa, &later.encode().unwrap())
        .unwrap();
    drop(catalog);
    let snapshot = dir.snapshot(&sa).unwrap();
    assert_eq!(
        snapshot.entries[&existing.scope.digest()].state,
        State::Subscribed
    );
    assert_eq!(
        snapshot.entries[&later.scope.digest()].state,
        State::Capacity
    );
    assert!(dir.publish(&sa, &later).is_err());
    assert!(!dir.path(later.scope).unwrap().exists());
    dir.enroll(&sa, existing.scope).unwrap();
}

#[test]
fn reader_publication_cannot_leave_orphan_storage() {
    let lab = Lab::new(&[Role::Reader]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let dir = Directory::new(&lab.paths[0], bootstrap.trust());
    assert!(
        dir.open_application(&session, "accounts", "data", 1)
            .is_err()
    );
    assert!(!dir.root.exists());
}

#[test]
fn catalog_writers_are_independent_when_another_writer_is_demoted() {
    let p = Pair::new();
    let a = p.lab.bootstrap(0);
    let b = p.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let da = p.directory(0);
    let db = p.directory(1);
    da.open_application(&sa, "first", "data", 1).unwrap();
    let source = da.catalog(&sa).unwrap();
    let mut target = db.catalog(&sb).unwrap();
    for id in source.inventory(&sa).unwrap() {
        target
            .ingest(&sb, source.wire(&sa, id).unwrap().unwrap())
            .unwrap();
    }
    drop(source);
    drop(target);
    let own = db.open_application(&sb, "second", "data", 1).unwrap();
    let catalog = db.catalog(&sb).unwrap();
    let records = catalog.catalog(&sb).unwrap();
    assert!(
        records
            .iter()
            .find(|r| r.header().actor == sb.actor())
            .unwrap()
            .header()
            .parents
            .is_empty()
    );
    drop(catalog);
    drop(sa);
    drop(sb);
    p.lab.rotate(&[Role::Reader, Role::Writer]);
    let b = p.lab.bootstrap(1);
    let sb = Session::open(&b).unwrap();
    assert_eq!(
        db.snapshot(&sb).unwrap().entries[&own.digest()].state,
        State::Subscribed
    );
    db.open_application(&sb, "third", "data", 1).unwrap();
}

#[test]
fn withdrawn_catalog_declarations_keep_only_retained_slots_and_preserve_exclusions_and_conflicts() {
    let p = Pair::new();
    let a = p.lab.bootstrap(0);
    let b = p.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let da = p.directory(0);
    let db = p.directory(1);
    let kept = da
        .open_application(&sa, "accounts", "activations", 1)
        .unwrap();
    let late = Descriptor::application(a.trust(), "unretained", "data", 1).unwrap();
    let conflict = Descriptor::application(a.trust(), "accounts", "activations", 2).unwrap();
    let mut source = da.catalog(&sa).unwrap();
    let mut target = db.catalog(&sb).unwrap();
    for id in source.inventory(&sa).unwrap() {
        target
            .ingest(&sb, source.wire(&sa, id).unwrap().unwrap())
            .unwrap();
    }
    drop(target);
    assert!(
        db.subscriptions(&sb, &Policy::default())
            .unwrap()
            .contains_key(&kept.digest())
    );
    // Both declarations were validly signed before withdrawal, but neither
    // has been projected or enrolled on this peer yet.
    let late_receipt = source.commit_catalog(&sa, &late.encode().unwrap()).unwrap();
    let conflict_receipt = source
        .commit_catalog(&sa, &conflict.encode().unwrap())
        .unwrap();
    let late_wire = source
        .wire(&sa, &late_receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    let conflict_wire = source
        .wire(&sa, &conflict_receipt.id)
        .unwrap()
        .unwrap()
        .to_vec();
    drop(source);
    drop(sa);
    drop(sb);
    p.lab.rotate(&[Role::Reader, Role::Writer]);
    let b = p.lab.bootstrap(1);
    let sb = Session::open(&b).unwrap();
    let mut target = db.catalog(&sb).unwrap();
    target.ingest(&sb, &late_wire).unwrap();
    drop(target);
    let snapshot = db.snapshot(&sb).unwrap();
    assert_eq!(snapshot.entries[&kept.digest()].state, State::Subscribed);
    assert!(!snapshot.entries.contains_key(&late.scope.digest()));
    assert!(!db.path(late.scope).unwrap().exists());
    let excluded = Policy {
        enabled: true,
        excluded: BTreeSet::from([kept.digest()]),
    };
    assert!(
        !db.subscriptions(&sb, &excluded)
            .unwrap()
            .contains_key(&kept.digest())
    );
    assert!(!db.permits(&sb, &excluded, kept.digest(), 0).unwrap());
    let mut target = db.catalog(&sb).unwrap();
    target.ingest(&sb, &conflict_wire).unwrap();
    drop(target);
    assert_eq!(
        db.snapshot(&sb).unwrap().entries[&kept.digest()].state,
        State::Conflict
    );
    assert!(
        !db.permits(&sb, &Policy::default(), kept.digest(), 0)
            .unwrap()
    );
}

/// The flags `action` leaves from `state`, without `subscribed`: everything the world
/// [`subscription_transition`] builds depends on.
fn subscription_flags(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = subscription_transition_flags(state, action);
    next.remove("subscribed");
    next
}

/// `state` after `action`'s flag update, before the world it describes is observed: its
/// `subscribed` is still `state`'s.
fn subscription_transition_flags(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Discover" => {
            next.insert("known", 1);
        }
        "Conflict" => {
            next.insert("conflict", 1);
        }
        "Exclude" => {
            next.insert("excluded", 1);
        }
        "Include" => {
            next.insert("excluded", 0);
        }
        "Disable" => {
            next.insert("enabled", 0);
        }
        "Enable" => {
            next.insert("enabled", 1);
        }
        "Quota" => {
            next.insert("overflow", 1);
        }
        _ => panic!("unknown model action"),
    }
    next
}

fn subscription_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let p = Pair::new();
    let a = p.lab.bootstrap(0);
    let b = p.lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let dir = p.directory(0);
    let descriptor = Descriptor::application(a.trust(), "accounts", "proposals", 1).unwrap();
    let scope = descriptor.scope;
    let mut catalog = dir.catalog(&sa).unwrap();
    let mut next = subscription_transition_flags(state, action);
    if next["known"] == 1 {
        catalog
            .commit_catalog(&sa, &descriptor.encode().unwrap())
            .unwrap();
    }
    if next["conflict"] == 1 {
        let mut other = descriptor;
        other.version = 2;
        let wire = crate::table::tests::seal(
            &sb,
            dir.catalog_scope(),
            crate::crypto::Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            &other.encode().unwrap(),
        )
        .unwrap();
        catalog.ingest(&sa, &wire).unwrap();
    }
    if next["overflow"] == 1 {
        for n in 0..=PER_CREATOR {
            let other =
                Descriptor::application(a.trust(), "extra", &format!("table{n}"), 1).unwrap();
            catalog
                .commit_catalog(&sa, &other.encode().unwrap())
                .unwrap();
        }
    }
    drop(catalog);
    let policy = Policy {
        enabled: next["enabled"] == 1,
        excluded: if next["excluded"] == 1 {
            BTreeSet::from([scope.digest()])
        } else {
            BTreeSet::new()
        },
    };
    next.insert(
        "subscribed",
        i64::from(
            dir.subscriptions(&sa, &policy)
                .unwrap()
                .contains_key(&scope.digest()),
        ),
    );
    next
}

#[test]
fn all_subscription_policy_interleavings_conform_and_mutants_fail() {
    let model = aterm_models_db::catalog_subscription_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let steps: Vec<_> = aterm_spec::interp::reachable_states(&model)
        .into_iter()
        .flat_map(|state| {
            model
                .actions
                .iter()
                .filter(|action| model.action_enabled(action.name, &state))
                .map(|action| (state.clone(), action.name))
                .collect::<Vec<_>>()
        })
        .collect();
    // Steps that leave the same flags build the same world and observe the same state:
    // project one step per world, every world on its own core.
    let mut first = BTreeMap::new();
    for (state, action) in &steps {
        first
            .entry(subscription_flags(state, action))
            .or_insert((state, *action));
    }
    let first: Vec<_> = first.into_iter().collect();
    let projected: BTreeMap<_, _> = first
        .iter()
        .map(|(flags, _)| flags.clone())
        .zip(aterm_spec::check::each_on(
            aterm_spec::fs_lane::PLACES,
            &first,
            |(_, (state, action))| subscription_transition(state, action),
        ))
        .collect();
    for (state, action) in &steps {
        let next = &projected[&subscription_flags(state, action)];
        assert!(
            model.successors(action, state).contains(next),
            "{action}: {state:?} -> {next:?}"
        );
    }
    let mut bad = projected[&subscription_flags(&model.init_state(), "Exclude")].clone();
    bad.insert("subscribed", 1);
    assert!(
        !model
            .successors("Exclude", &model.init_state())
            .contains(&bad)
    );
    assert!(!model.check_invariant("ExclusionBlocks", &bad));
}
