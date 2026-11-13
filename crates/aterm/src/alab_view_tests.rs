// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// With no address row saying it, a mailbox key names its login by derivation: each listed
/// login maps the key its address key derives to the login, lower-cased, and a line that is
/// no address maps nothing. (A row that says a login now keys it instead:
/// `aterm_db::alab::mail`'s `a_login_names_the_mailbox_of_the_row_that_says_it_now`.)
/// NEGATIVE CONTROL: with `mailbox_logins` keying by the login itself, the lookup by the
/// derived key finds nothing.
#[test]
fn a_mailbox_key_names_the_login_it_derives_from() {
    let logins = ["Hub@Example.test".to_owned(), "not an address".to_owned()];
    let named = mailbox_logins(&AddressIndex::default(), &logins);
    assert_eq!(named.len(), 1);
    let key = aterm_db::typed::keys::mailbox_pk(
        &aterm_db::typed::keys::address_pk("hub@example.test").unwrap(),
    )
    .unwrap();
    assert_eq!(
        named.get(&key).map(String::as_str),
        Some("hub@example.test")
    );
}

/// A test process holds no lab, so no test reads this Mac's registry.
/// NEGATIVE CONTROL: with `holds` returning `true`, this fails.
#[test]
fn a_test_holds_no_lab() {
    assert!(!holds());
}

/// The registry names a mailbox key's login by its own `login` row (step A9), with no
/// `ai-mailbox.toml`; the file's logins name only a key the registry names none of, and an
/// address the lab places in a registry mailbox gets that mailbox's login.
/// NEGATIVE CONTROL: with `logins_of` letting the file's login win, the registry key reads
/// `old@example.test` and the first assertion fails.
#[test]
fn a_registry_mailbox_names_its_own_login() {
    let mailboxes = [aterm_db::alab::mail::Mailbox {
        pk: "mb-registry".into(),
        label: "proton-1".into(),
        login: "Owner@example.test".into(),
        login_pk: "ad-0000000000000000".into(),
        service: Some("proton".into()),
        code_path: Some("webmail".into()),
        webmail: None,
        receives: vec!["*".into()],
        held: vec![mail::Credential::Password, mail::Credential::Totp],
    }];
    let mail = lab_mail(&mailboxes);
    assert_eq!(mail[0].login, "owner@example.test");
    assert_eq!(mail[0].held, [Part::Password, Part::Totp]);
    let toml = BTreeMap::from([
        ("mb-registry".to_owned(), "old@example.test".to_owned()),
        ("mb-file".to_owned(), "file@example.test".to_owned()),
    ]);
    let logins = logins_of(&mail, &toml);
    assert_eq!(logins["mb-registry"], "owner@example.test");
    assert_eq!(logins["mb-file"], "file@example.test");
    let placed = BTreeMap::from([("ai1@example.test".to_owned(), "mb-registry".to_owned())]);
    assert_eq!(
        named_mailboxes(&placed, &logins_of(&mail, &BTreeMap::new()))["ai1@example.test"],
        "owner@example.test"
    );
}

pub(super) fn nothing_held() -> Held<Inventory> {
    Held {
        projected: None,
        looked: None,
        pending: true,
    }
}

#[cfg(unix)]
#[test]
fn current_local_policy_withholds_a_cached_projection_without_erasing_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let config = dir.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"[alab]\nroster = true\n", 0o600).unwrap();
    let mut held = nothing_held();
    look(&mut held, Ok(Some(naming(Some("m3")))), Instant::now());
    let cache = Cache {
        paths: Paths::for_home(dir.path()),
        config: config.clone(),
        settled: None,
        held: Mutex::new(held),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    };
    let observed = cache.projection();
    let original = observed.lab().unwrap();
    let revision = cache
        .held
        .lock()
        .unwrap()
        .projected
        .as_ref()
        .unwrap()
        .source;
    assert!(!cache.projection().pending());

    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(observed.names_this_mac() && !observed.pending());
    assert!(
        cache.projection().lab().is_none(),
        "an unavailable current policy must not expose the cached lab"
    );
    assert!(
        cache.projection().pending(),
        "unknown policy cannot select a sheet instead"
    );
    assert!(Arc::ptr_eq(
        cache.held.lock().unwrap().lab().unwrap(),
        &original
    ));
    assert_eq!(
        cache
            .held
            .lock()
            .unwrap()
            .projected
            .as_ref()
            .unwrap()
            .source,
        revision
    );

    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    for invalid in ["[alab", "[alab]\nroster = 'off'\n"] {
        std::fs::write(&config, invalid).unwrap();
        assert!(cache.projection().lab().is_none());
        assert!(cache.projection().pending());
    }
    std::fs::write(&config, "[alab]\nroster = false\n").unwrap();
    cache.refresh_with(|| panic!("excluded policy must not read the source"));
    assert!(
        cache
            .held
            .lock()
            .unwrap()
            .lab()
            .is_some_and(|lab| Arc::ptr_eq(lab, &original)),
        "a disabled background refresh must preserve the view for re-enablement"
    );
    assert!(cache.projection().lab().is_none());
    assert!(
        !cache.projection().pending(),
        "explicit off is a known policy"
    );
    std::fs::write(&config, "[alab]\nroster = true\n").unwrap();
    assert!(Arc::ptr_eq(&cache.projection().lab().unwrap(), &original));
    assert!(!cache.projection().pending());
    std::fs::remove_file(&config).unwrap();
    assert!(Arc::ptr_eq(&cache.projection().lab().unwrap(), &original));
    assert!(
        !cache.projection().pending(),
        "real absence keeps its defaults"
    );
}

/// A projection whose facts row binds this Mac to a machine labelled `label` (`None`: the
/// label is conflicted).
pub(super) fn naming(label: Option<&str>) -> Projected<Inventory> {
    Projected {
        lab: Arc::new(harness_lab(
            &naming_view(label, MachineNickname::Unset),
            false,
            &BTreeMap::new(),
        )),
        source: Inventory([7; 32]),
    }
}

fn naming_view(label: Option<&str>, nickname: MachineNickname) -> LabView {
    let mut view = LabView::default();
    view.me = Some("pk-me".into());
    view.machines.insert(
        "pk-me".into(),
        aterm_db::alab::view::Machine {
            pk: "pk-me".into(),
            label: label.map(str::to_owned),
            nickname,
            aliases: Vec::new(),
            live: true,
        },
    );
    view
}

#[test]
fn equal_machine_labels_do_not_transfer_a_preference_between_registry_keys() {
    use aterm_agent::harness::{accounts::Class, alab_roster};
    use aterm_db::alab::view::{Account, Machine, Preference};

    let mut view = naming_view(Some("m3"), MachineNickname::Unset);
    view.machines.insert(
        "pk-other".into(),
        Machine {
            pk: "pk-other".into(),
            label: Some("m3".into()),
            nickname: MachineNickname::Unset,
            aliases: Vec::new(),
            live: true,
        },
    );
    view.accounts.insert(
        "account-one".into(),
        Account {
            pk: "account-one".into(),
            realm: Some("claude".into()),
            label: Some("claude-one".into()),
            legacy_label: None,
            login: Some("one@example.test".into()),
            identity: None,
            status: Some("active".into()),
            region: None,
            flow: None,
            browser: None,
            live: true,
        },
    );
    view.preferences.push(Preference {
        machine: "pk-other".into(),
        account: "account-one".into(),
        rank: Some(1),
    });

    let project =
        |view: &LabView| harness_lab_with_placed(view, &BTreeMap::new(), true, &BTreeMap::new());
    let foreign = project(&view);
    assert_eq!(foreign.class_of(&foreign.accounts[0]), Class::Pool);
    assert!(
        alab_roster::fleet_for(&foreign, Cli::Claude, None)
            .unwrap()
            .accounts
            .is_empty(),
        "a different registry key must not enroll an account through a shared label"
    );

    view.preferences[0].machine = "pk-me".into();
    let own = project(&view);
    assert_eq!(own.class_of(&own.accounts[0]), Class::Preferred);
    assert_eq!(
        alab_roster::fleet_for(&own, Cli::Claude, None)
            .unwrap()
            .accounts
            .len(),
        1,
        "the same label collision must not suppress this machine's own preference"
    );
}

#[test]
fn a_bound_registry_name_creates_the_same_local_mirror_the_cli_and_menu_read() {
    use aterm_agent::machine_name::{Origin, ensure_from_registry_for_test, file};
    use aterm_net::fleet_member::name_record::NickClass;
    use aterm_types::machine_name::{Class, IMPLIED_SINCE};
    for automatic in [false, true] {
        let view = naming_view(
            Some("m86"),
            MachineNickname::Named {
                word: "MissBunny".into(),
                automatic,
            },
        );
        let lab = harness_lab(&view, false, &BTreeMap::new());
        let expected = registry_name(&view).unwrap().unwrap();
        assert_eq!(lab.machine_name, Some(Ok(expected.clone())));
        let state = aterm_tempfile::tempdir().unwrap();
        aterm_uds::private_file::ensure_private_dir(state.path()).unwrap();
        let (name, origin) = ensure_from_registry_for_test(
            state.path(),
            || RegistryNameRead::Bound(lab.machine_name.unwrap()),
            || panic!("the bound registry row outranks the roster"),
            |_| panic!("the registry already supplies the nickname"),
            1_800_000_000_000,
        )
        .unwrap();
        assert_eq!(origin, Origin::Registry);
        assert_eq!(name.title(), "m86 MissBunny");
        assert_eq!(
            (name.class, name.since, name.rev, name.migration),
            (Class::Seeded, IMPLIED_SINCE, 0, None)
        );
        assert_eq!(
            name.nick_class,
            if automatic {
                NickClass::Auto
            } else {
                NickClass::Chosen
            }
        );
        assert_eq!(file::read(state.path()).unwrap(), Some(name));
    }
}

#[test]
fn only_a_valid_canonical_bound_registry_name_can_create_a_mirror() {
    assert_eq!(registry_name(&LabView::default()), None);
    let zero = registry_name(&naming_view(Some("m0"), MachineNickname::Unset))
        .unwrap()
        .unwrap();
    assert_eq!(zero.num.get(), 0);
    let mut retired = naming_view(Some("m86"), MachineNickname::Unset);
    retired.machines.get_mut("pk-me").unwrap().live = false;
    assert_eq!(registry_name(&retired), None);
    for view in [
        naming_view(None, MachineNickname::Unset),
        naming_view(Some("m40-3fa2"), MachineNickname::Unset),
        naming_view(Some("M86"), MachineNickname::Unset),
        naming_view(Some("m86"), MachineNickname::Conflict),
        naming_view(
            Some("m86"),
            MachineNickname::Named {
                word: "m7".into(),
                automatic: false,
            },
        ),
    ] {
        assert!(registry_name(&view).unwrap().is_err(), "{view:?}");
    }
    assert!(
        registry_name(&naming_view(Some("m86"), MachineNickname::Unset))
            .unwrap()
            .unwrap()
            .nick
            .is_none()
    );
}

#[test]
fn naming_policy_and_pending_status_are_read_from_one_retained_snapshot() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("aterm.toml");
    let mut held = nothing_held();
    look(&mut held, Ok(Some(naming(Some("m86")))), Instant::now());
    let cache = Cache {
        paths: Paths::for_home(root.path()),
        config: config.clone(),
        settled: None,
        held: Mutex::new(held),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    };
    let expected = cache.projection().machine_name();
    assert!(matches!(&expected, RegistryNameRead::Bound(Ok(_))));
    look(
        &mut cache.held.lock().unwrap(),
        Err("temporarily unavailable".into()),
        Instant::now(),
    );
    assert_eq!(cache.projection().machine_name(), expected);
    aterm_uds::atomic_file::write(&config, b"[alab\n", 0o600).unwrap();
    assert_eq!(cache.projection().machine_name(), RegistryNameRead::Pending);
    aterm_uds::atomic_file::write(&config, b"[alab]\nroster = false\n", 0o600).unwrap();
    assert_eq!(cache.projection().machine_name(), RegistryNameRead::Unbound);
    std::fs::remove_file(&config).unwrap();
    assert_eq!(cache.projection().machine_name(), expected);
    *cache.held.lock().unwrap() = nothing_held();
    assert_eq!(cache.projection().machine_name(), RegistryNameRead::Pending);
    cache.held.lock().unwrap().pending = false;
    assert_eq!(cache.projection().machine_name(), RegistryNameRead::Unbound);
    for read in [
        Err("first read unavailable".into()),
        Err("source history incomplete".into()),
    ] {
        look(&mut cache.held.lock().unwrap(), read, Instant::now());
        assert_eq!(
            cache.projection().machine_name(),
            RegistryNameRead::Pending,
            "an unread first registry is not proof that no binding exists"
        );
        let held = cache.held.lock().unwrap();
        assert!(held.pending);
        assert!(due(
            held.looked,
            held.pending,
            Instant::now() + RETRY_FAILED
        ));
    }
}

#[test]
fn an_unread_registry_keeps_both_consumers_pending_without_a_history_marker() {
    let root = aterm_tempfile::tempdir().unwrap();
    let marker_dir = root.path().join("alab");
    assert!(!marker_dir.exists());
    for read in [
        Err("registry is temporarily unreadable".to_owned()),
        Err("source history incomplete".to_owned()),
    ] {
        let mut held = nothing_held();
        look(&mut held, read, Instant::now());
        let cache = Cache {
            paths: Paths::for_home(root.path()),
            config: root.path().join("aterm.toml"),
            settled: None,
            held: Mutex::new(held),
            refreshing: Mutex::new(()),
            refresher: OnceLock::new(),
        };
        let observed = cache.projection();
        assert_eq!(observed.machine_name(), RegistryNameRead::Pending);
        assert!(
            observed.pending() && !observed.permits_sheet(),
            "an unread registry must not enable legacy account discovery"
        );
    }
}

/// Failed and partial reads preserve the exact projection and its source. Only a
/// successful complete read replaces the pair, and successful absence clears it.
#[test]
fn a_look_preserves_a_projection_and_keeps_unknown_absence_pending() {
    let now = Instant::now();
    let failed = || Err::<Option<Projected<Inventory>>, String>("custody unreadable".into());
    for (read, what) in [
        (failed(), "a failed read"),
        (Err("source history incomplete".into()), "a partial history"),
    ] {
        let mut held = nothing_held();
        look(&mut held, read, now);
        assert!(held.pending, "{what}");
        assert!(held.projected.is_none(), "{what}");
        assert_eq!(held.looked, Some(now), "{what}");
    }

    let mut held = nothing_held();
    look(&mut held, Ok(Some(naming(Some("m3")))), now);
    let lab = held.lab().cloned().expect("the projection is held");
    assert_eq!(
        lab.me.as_ref().and_then(|me| me.label.as_deref()),
        Some("m3")
    );
    assert_eq!(lab.me.as_ref().map(|me| me.key.as_str()), Some("pk-me"));
    assert!(!lab.settled && !held.pending);
    assert_eq!(held.projected.as_ref().unwrap().source, Inventory([7; 32]));
    for (read, what) in [
        (Err("source history incomplete".into()), "a partial history"),
        (failed(), "a failed read"),
    ] {
        look(&mut held, read, now);
        assert!(
            held.lab().is_some_and(|kept| Arc::ptr_eq(kept, &lab)),
            "{what} keeps the lab"
        );
        assert!(!held.pending, "{what}: a held lab is not pending");
        assert_eq!(
            held.projected.as_ref().unwrap().source,
            Inventory([7; 32]),
            "{what}"
        );
    }
    let mut next = naming(Some("m3"));
    Arc::make_mut(&mut next.lab).settled = true;
    next.source = Inventory([8; 32]);
    look(&mut held, Ok(Some(next)), now);
    assert!(
        held.lab().is_some_and(|lab| lab.settled),
        "a complete read installs local overlays with the new source"
    );
    assert_eq!(held.projected.as_ref().unwrap().source, Inventory([8; 32]));
    assert!(!Arc::ptr_eq(held.lab().unwrap(), &lab));
    look(&mut held, Ok(None), now);
    assert!(
        held.projected.is_none(),
        "absence clears the projection and its source together"
    );
    assert!(!held.pending, "a successful absence resolves uncertainty");
}

#[test]
fn current_commons_policy_hides_a_source_and_preserves_its_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let cache = Cache {
        paths: Paths::for_home(root.path()),
        config: root.path().join("aterm.toml"),
        settled: None,
        held: Mutex::new(nothing_held()),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    };
    cache.refresh_with(|| Ok(Some(naming(Some("m3")))));
    let original = cache.capture().1.unwrap();
    let exclusion = |scope: [u8; 32]| {
        format!(
            "[commons]\nexclude_tables = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(&scope)
        )
    };
    for (text, pending, scan) in [
        ("[commons]\nenabled = false\n".to_owned(), false, false),
        ("[swarm]\nenabled = false\n".to_owned(), false, false),
        (exclusion([0; 32]), false, true),
        (exclusion([7; 32]), false, true),
        ("[commons]\nenabled = 'yes'\n".to_owned(), true, false),
        (
            "[commons]\nexclude_tables = ['invalid']\n".to_owned(),
            true,
            false,
        ),
    ] {
        aterm_uds::atomic_file::write(&cache.config, text.as_bytes(), 0o600).unwrap();
        let (snapshot, projected) = cache.capture();
        assert!(projected.is_none());
        assert_eq!(snapshot.pending(), pending);
        assert_eq!(snapshot.disabled(), !pending);
        let called = std::cell::Cell::new(false);
        cache.refresh_with(|| {
            called.set(true);
            Ok(None)
        });
        assert_eq!(
            called.get(),
            scan,
            "only an enabled scope scan may discover an included successor"
        );
        let held = cache.held.lock().unwrap();
        assert!(Arc::ptr_eq(held.lab().unwrap(), &original.lab));
        assert_eq!(held.projected.as_ref().unwrap().source, original.source);
    }
    aterm_uds::atomic_file::write(&cache.config, b"", 0o600).unwrap();
    cache.refresh_with(|| {
        aterm_uds::atomic_file::write(&cache.config, exclusion([7; 32]).as_bytes(), 0o600).unwrap();
        Ok(None)
    });
    assert!(cache.capture().0.disabled());
    assert!(Arc::ptr_eq(
        cache.held.lock().unwrap().lab().unwrap(),
        &original.lab
    ));

    // A different table can become excluded while an older held table is included.
    aterm_uds::atomic_file::write(&cache.config, exclusion([9; 32]).as_bytes(), 0o600).unwrap();
    cache.refresh_with(|| {
        let mut next = naming(Some("m4"));
        next.source = Inventory([9; 32]);
        Ok(Some(next))
    });
    let kept = cache.capture().1.unwrap();
    assert!(Arc::ptr_eq(&kept.lab, &original.lab));
    assert_eq!(kept.source, original.source);
    aterm_uds::atomic_file::write(&cache.config, exclusion([7; 32]).as_bytes(), 0o600).unwrap();
    cache.refresh_with(|| {
        let mut next = naming(Some("m4"));
        next.source = Inventory([9; 32]);
        Ok(Some(next))
    });
    assert_eq!(
        cache.capture().1.unwrap().source,
        Inventory([9; 32]),
        "an excluded old generation must not freeze a permitted new one"
    );
}

#[test]
fn native_cache_projects_mail_and_settlement_from_its_captured_source() {
    use aterm_db::alab::registry::{ADDRESS, MAILBOX};
    use aterm_db::typed::{cell, keys};
    use aterm_json::json;
    use aterm_secrets::commons::roster::Role;

    let lab = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let table = registry.open().unwrap();
    let login = keys::address_pk("owner@example.test").unwrap();
    let alias = keys::address_pk("ai@example.test").unwrap();
    let mailbox = keys::mailbox_pk(&login).unwrap();
    let changes = [
        (
            &ADDRESS,
            login.as_str(),
            "addr",
            json!("owner@example.test"),
        ),
        (&ADDRESS, alias.as_str(), "addr", json!("ai@example.test")),
        (&ADDRESS, alias.as_str(), "mailbox", json!(mailbox)),
        (&MAILBOX, mailbox.as_str(), "login", json!(login)),
    ]
    .into_iter()
    .map(|(collection, pk, col, value)| {
        cell::change(
            collection,
            cell::CellKey {
                pk,
                col,
                elem: None,
            },
            &cell::CellValue::owner(1, value),
        )
        .unwrap()
    })
    .collect();
    registry.write(vec![changes]).unwrap();
    let config = paths.home.join("aterm.toml");
    let source = registry.observe_projection(&config).unwrap();
    let settled = paths.home.join("settled-observation");
    aterm_types::fs_restricted::ensure_private_dir(&settled).unwrap();
    aterm_uds::atomic_file::write(
        &settled.join("settled"),
        aterm_codec::hex::encode_lower(&table.digest()).as_bytes(),
        0o600,
    )
    .unwrap();
    let cache = Cache {
        paths: paths.clone(),
        config,
        settled: Some(settled.clone()),
        held: Mutex::new(Held {
            projected: None,
            looked: None,
            pending: true,
        }),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    };
    aterm_uds::atomic_file::write(
        &cache.config,
        format!(
            "[commons]\nexclude_tables = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(&table.digest())
        )
        .as_bytes(),
        0o600,
    )
    .unwrap();
    cache.refresh();
    assert!(
        cache.capture().0.pending(),
        "a cold excluded registry must not become proof of absence"
    );
    aterm_uds::atomic_file::write(&cache.config, b"", 0o600).unwrap();
    cache.refresh();
    let (snapshot, projected) = cache.capture();
    let projected = projected.unwrap();
    assert!(Arc::ptr_eq(&snapshot.lab().unwrap(), &projected.lab));
    assert_eq!(projected.source.revision(), source.revision());
    assert_eq!(projected.source.source().mailboxes()[0].pk, mailbox);
    assert_eq!(
        projected.lab.mailboxes["ai@example.test"],
        "owner@example.test"
    );
    assert!(projected.lab.settled);
    aterm_uds::atomic_file::write(
        &settled.join("settled"),
        aterm_codec::hex::encode_lower(&[0; 32]).as_bytes(),
        0o600,
    )
    .unwrap();
    cache.refresh();
    assert!(
        !cache.capture().1.unwrap().lab.settled,
        "a different table's settlement cannot attach to this source"
    );
}

#[test]
fn a_new_installation_is_an_absent_source_without_custody() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let cache: &'static Cache = Box::leak(Box::new(Cache {
        paths: paths.clone(),
        config: paths.home.join("aterm.toml"),
        settled: None,
        held: Mutex::new(Held {
            projected: None,
            looked: None,
            pending: true,
        }),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    }));
    cache.refresh();
    let absent = alab_roster::Source::capture(&cache);
    assert!(
        matches!(absent.snapshot(), SourceSnapshot::Absent),
        "{:?}",
        absent.snapshot()
    );
    assert!(absent.snapshot().permits_sheet());
    assert_eq!(cache.projection().machine_name(), RegistryNameRead::Unbound);
    assert!(
        absent.native_guard().is_err(),
        "absence grants no native receipt"
    );
    assert!(!paths.dir.exists(), "the observation created custody");
}

#[test]
fn native_cache_does_not_convert_missing_projection_to_fallback_authority() {
    use aterm_db::{
        catalog::{Descriptor, Directory},
        table::Session,
    };
    use aterm_secrets::commons::roster::Role;

    let fixture = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
    let paths = &fixture.paths[0];
    let cache: &'static Cache = Box::leak(Box::new(Cache {
        paths: paths.clone(),
        config: paths.home.join("aterm.toml"),
        settled: None,
        held: Mutex::new(Held {
            projected: None,
            looked: None,
            pending: true,
        }),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    }));
    cache.refresh();
    assert!(
        cache.capture().0.pending(),
        "missing catalog is unavailable"
    );

    let bootstrap = fixture.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let descriptor = Descriptor::application(bootstrap.trust(), "notes", "pages", 1).unwrap();
    directory.publish(&session, &descriptor).unwrap();
    let catalog = directory.path(directory.catalog_scope()).unwrap();
    drop(session);

    assert!(matches!(
        Registry::at(paths.clone()).observe_local_source(&cache.config),
        Ok(aterm_db::alab::LocalSourceRead::Absent(_))
    ));
    cache.refresh();
    let absent = alab_roster::Source::capture(&cache);
    assert!(absent.snapshot().pending());
    assert!(!absent.snapshot().disabled());
    assert!(absent.snapshot().lab().is_none());
    assert!(
        absent.native_guard().is_err(),
        "native absence needs separate admission before fallback"
    );

    std::fs::rename(catalog, paths.home.join("parked-catalog")).unwrap();
    cache.refresh();
    assert!(
        cache.capture().0.pending(),
        "lost custody remains unavailable"
    );
}

#[test]
fn native_receipt_guard_rechecks_history_policy_and_overlays_before_account_save() {
    use aterm_db::typed::cell;
    use aterm_secrets::commons::roster::Role;

    for change in 0..8 {
        let fixture = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
        let paths = &fixture.paths[0];
        let registry = Registry::at(paths.clone());
        let table = registry.open().unwrap();
        let config = paths.home.join("aterm.toml");
        aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
        let settled = paths.home.join("settlement");
        aterm_uds::private_file::create_private_dir(&settled).unwrap();
        let write_settlement = |digest: &[u8]| {
            aterm_uds::atomic_file::write(
                &settled.join("settled"),
                aterm_codec::hex::encode_lower(digest).as_bytes(),
                0o600,
            )
            .unwrap()
        };
        write_settlement(&table.digest());
        if change == 5 {
            fixture.rotate(&[Role::Reader]);
        }
        let cache: &'static Cache = Box::leak(Box::new(Cache {
            paths: paths.clone(),
            config: config.clone(),
            settled: Some(settled.clone()),
            held: Mutex::new(Held {
                projected: None,
                looked: None,
                pending: true,
            }),
            refreshing: Mutex::new(()),
            refresher: OnceLock::new(),
        }));
        cache.refresh();
        let captured = alab_roster::Source::capture(&cache);
        let source = captured.native_guard().unwrap();
        let old_lab = captured.snapshot().lab().unwrap();
        match change {
            1 | 4 => {
                registry
                    .write(vec![vec![
                        cell::change(
                            &aterm_db::alab::registry::MODEL,
                            cell::CellKey {
                                pk: "invisible-model",
                                col: "row",
                                elem: None,
                            },
                            &cell::CellValue::automatic(1, aterm_json::json!("live")),
                        )
                        .unwrap(),
                    ]])
                    .unwrap();
                assert_eq!(
                    cache.lab_for_source(registry.observe_projection(&config).unwrap().source()),
                    old_lab
                );
                if change == 4 {
                    cache.refresh();
                }
            }
            2 => {
                aterm_uds::atomic_file::write(&config, b"[alab]\nroster = false\n", 0o600).unwrap()
            }
            3 => write_settlement(&[0; 32]),
            6 | 7 => {
                let recovery = paths.dir.join("commons/recovery/tables");
                aterm_types::fs_restricted::ensure_private_dir(&recovery).unwrap();
                let recovery_table = recovery.join(aterm_codec::hex::encode_lower(&table.digest()));
                aterm_types::fs_restricted::ensure_private_dir(&recovery_table).unwrap();
                if change == 6 {
                    aterm_uds::atomic_file::write(
                        &recovery_table.join("pending"),
                        b"unclaimed recovery data",
                        0o600,
                    )
                    .unwrap();
                }
            }
            _ => {}
        }
        let store = aterm_agent::harness::accounts::Store::at(&paths.home.join("account-test"));
        let roster = paths.home.join("accounts.toml");
        let result =
            alab_roster::test_publish_source_roster(&store, &roster, "# admitted\n", source);
        let admitted = matches!(change, 0 | 5 | 7);
        assert_eq!(result.is_ok(), admitted, "change={change}: {result:?}");
        assert_eq!(roster.exists(), admitted);
        assert_eq!(store.dir.join("sync-commit.json").exists(), admitted);
        assert_eq!(
            cache.held.lock().unwrap().looked.is_some(),
            admitted || change == 4,
            "only the rejected cached revision is invalidated"
        );
        if admitted {
            let caller = paths.home.join("caller.toml");
            let excluded = format!(
                "[commons]\nexclude_tables = [\"{}\"]\n",
                aterm_codec::hex::encode_lower(&table.digest())
            );
            for policy in [
                "[alab]\nroster = false\n",
                "[commons]\nenabled = false\n",
                excluded.as_str(),
            ] {
                aterm_uds::atomic_file::write(&caller, policy.as_bytes(), 0o600).unwrap();
                let blocked = alab_roster::Source::capture_for(&cache, &caller);
                assert!(blocked.snapshot().disabled());
                assert!(blocked.native_guard().is_err());
            }
            aterm_uds::atomic_file::write(&caller, b"", 0o600).unwrap();
            let allowed = alab_roster::Source::capture_for(&cache, &caller);
            aterm_uds::atomic_file::write(&caller, excluded.as_bytes(), 0o600).unwrap();
            assert!(
                alab_roster::test_publish_source_roster(
                    &store,
                    &roster,
                    "# refused\n",
                    allowed.native_guard().unwrap()
                )
                .is_err()
            );
            assert_eq!(std::fs::read_to_string(&roster).unwrap(), "# admitted\n");
        }
    }
}

#[test]
fn caller_policy_is_observed_even_without_a_host_projection() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("aterm.toml");
    let caller = root.path().join("caller.toml");
    let cache: &'static Cache = Box::leak(Box::new(Cache {
        paths: Paths::for_home(root.path()),
        config,
        settled: None,
        held: Mutex::new(Held {
            projected: None,
            looked: Some(Instant::now()),
            pending: false,
        }),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    }));
    aterm_uds::atomic_file::write(&caller, b"[alab]\nroster=false\n", 0o600).unwrap();
    assert!(
        alab_roster::Source::capture_for(&cache, &caller)
            .snapshot()
            .disabled()
    );
    aterm_uds::atomic_file::write(&caller, b"[alab", 0o600).unwrap();
    assert!(
        alab_roster::Source::capture_for(&cache, &caller)
            .snapshot()
            .pending()
    );
}

#[test]
fn equal_visible_labs_keep_the_source_of_each_complete_refresh() {
    let root = aterm_tempfile::tempdir().unwrap();
    let cache = Cache {
        paths: Paths::for_home(root.path()),
        config: root.path().join("aterm.toml"),
        settled: None,
        held: Mutex::new(nothing_held()),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    };
    cache.refresh_with(|| Ok(Some(naming(Some("m3")))));
    let (snapshot, captured) = cache.capture();
    let captured = captured.unwrap();
    assert!(Arc::ptr_eq(&snapshot.lab().unwrap(), &captured.lab));
    let mut later = naming(Some("m3"));
    later.source = Inventory([9; 32]);
    cache.refresh_with(|| Ok(Some(later)));
    let current = cache.capture().1.unwrap();
    assert_eq!(captured.lab, current.lab);
    assert_ne!(captured.source, current.source);
    assert_eq!(captured.source, Inventory([7; 32]));
    cache.refresh_with(|| Err("temporary native source failure".into()));
    let kept = cache.capture().1.unwrap();
    assert!(Arc::ptr_eq(&kept.lab, &current.lab));
    assert_eq!(kept.source, current.source);
    aterm_uds::atomic_file::write(&cache.config, b"[alab]\nroster = false\n", 0o600).unwrap();
    let (disabled, hidden) = cache.capture();
    assert!(disabled.disabled() && hidden.is_none());
    aterm_uds::atomic_file::write(&cache.config, b"[alab\n", 0o600).unwrap();
    let (pending, hidden) = cache.capture();
    assert!(pending.pending() && hidden.is_none());
}

/// A caller looks at the first look and, while a failed look left the source pending, again
/// once 15 seconds passed; otherwise the refresher looks.
/// NEGATIVE CONTROL: with `failed` ignored, `` the refresher looks while a lab is held ``
/// fails; with `>` for `>=`, `` 15 s after a pending look `` fails.
#[test]
fn a_caller_looks_first_and_again_only_15_s_after_a_pending_look() {
    let at = Instant::now();
    let after = |secs| at + Duration::from_secs(secs);
    assert!(
        !due(Some(at), false, after(3600)),
        "the refresher looks while a lab is held"
    );
    assert!(due(None, false, at), "the first look");
    assert!(due(None, true, at), "the first look");
    assert!(!due(Some(at), true, after(14)), "14 s after a pending look");
    assert!(due(Some(at), true, after(15)), "15 s after a pending look");
    assert!(!due(Some(after(1)), true, at), "a look stamped after now");
}

/// A completed round settles a table only where its witness counts an operation of one of
/// the registry's generations, not settled yet, that the registry holds here, and the held
/// table, a read of custody and the catalog, is read only for such a witness.
/// NEGATIVE CONTROL: without the `count > 0` filter, `` an empty witness `` settles the held
/// table; without the `is_generation` filter, `` another application `` reads the held table.
#[test]
fn a_round_settles_only_a_witness_that_counts_an_operation_of_the_table_held_here() {
    use aterm_secrets::commons::journal::Trust;
    let trust = Trust {
        network: [3; 16],
        authority: [4; 32],
        genesis: [5; 32],
    };
    let registry = |compiled: &aterm_db::att::Compiled| compiled.descriptor(trust).unwrap().scope;
    let held = registry(&aterm_db::att::REGISTRY);
    let next = registry(&aterm_db::att::REGISTRY_2);
    let other = aterm_db::catalog::Descriptor::application(trust, "aterm.lab", "tables", 1)
        .unwrap()
        .scope;
    let unread = || -> Option<[u8; 32]> { panic!("the held table is read only for a witness") };
    let unsettled = |_: &[u8; 32]| false;
    assert_eq!(
        witness_table(Some((0, held)), unsettled, || Some(held.digest())),
        None,
        "an empty witness"
    );
    assert_eq!(
        witness_table(Some((3, next)), unsettled, || Some(held.digest())),
        None,
        "another generation"
    );
    assert_eq!(
        witness_table(Some((3, held)), unsettled, || None),
        None,
        "no table held here"
    );
    assert_eq!(witness_table(None, unsettled, unread), None, "no witness");
    assert_eq!(
        witness_table(Some((0, held)), unsettled, unread),
        None,
        "an empty witness"
    );
    assert_eq!(
        witness_table(Some((3, other)), unsettled, unread),
        None,
        "another application"
    );
    assert_eq!(
        witness_table(Some((3, held)), |table| *table == held.digest(), unread),
        None,
        "settled already"
    );
    assert_eq!(
        witness_table(Some((3, held)), unsettled, || Some(held.digest())),
        Some(held.digest())
    );
}

/// A shipped build holds a lab only in the person's own environment: a build tool's run, a
/// scratch state root and CI each hold it off, a variable that only resembles one does not,
/// and a debug build holds none whatever its environment.
/// NEGATIVE CONTROL: with the `ATERM_STATE_HOME` clause dropped, the scratch-root row holds a
/// lab: `` a release build with ATERM_STATE_HOME set ``.
#[test]
fn a_shipped_build_holds_a_lab_only_in_the_persons_own_environment() {
    let env = |set: &'static [&'static str]| {
        move |key: &str| set.contains(&key).then(|| std::ffi::OsString::from("1"))
    };
    const LOOKALIKES: &[&str] = &[
        "XDG_STATE_HOME",
        "ATERM_STATE_HOME_X",
        "CIRCLECI",
        "CARGO_HOME",
    ];
    assert!(
        lab_hooks_on(true, env(LOOKALIKES)),
        "only lookalikes are set"
    );
    assert!(
        !lab_hooks_on(true, env(&["XDG_STATE_HOME", "CARGO_MANIFEST_DIR"])),
        "a release build with CARGO_MANIFEST_DIR set"
    );
    assert!(
        !lab_hooks_on(true, env(&["ATERM_STATE_HOME_X", "ATERM_STATE_HOME"])),
        "a release build with ATERM_STATE_HOME set"
    );
    assert!(
        !lab_hooks_on(true, env(&["CIRCLECI", "CI"])),
        "a release build with CI set"
    );
    assert!(!lab_hooks_on(false, env(&[])), "a debug build");
}

/// Only admitted `[alab] roster = false` is an explicit Off switch.
#[test]
fn the_roster_source_is_on_unless_turned_off() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let config = dir.path().join("aterm.toml");
    assert!(matches!(roster_policy(&config), LocalRosterPolicy::Enabled));
    aterm_uds::atomic_file::write(&config, b"[alab]\nroster = true\n", 0o600).unwrap();
    assert!(matches!(roster_policy(&config), LocalRosterPolicy::Enabled));
    std::fs::write(&config, "[alab]\nroster = false\n").unwrap();
    assert!(matches!(
        roster_policy(&config),
        LocalRosterPolicy::Disabled
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(
            matches!(roster_policy(&config), LocalRosterPolicy::Unavailable),
            "world-writable aterm.toml must not be trusted"
        );
    }
}

/// The lab's placements are named from the mailbox logins of each look: an address whose
/// mailbox a look names gets that login, and one whose mailbox it does not name gets none,
/// so a mailbox added to `ai-mailbox.toml` is seen with the registry unchanged.
/// NEGATIVE CONTROL: naming every placement from the first look's logins leaves the hub's
/// address unnamed after the hub is added (`seen once added`).
#[test]
fn placements_are_named_from_each_looks_logins() {
    let placed: BTreeMap<String, String> = [
        ("a@example.test".to_owned(), "mb-hub".to_owned()),
        ("b@example.test".to_owned(), "mb-side".to_owned()),
    ]
    .into();
    let side: BTreeMap<String, String> =
        [("mb-side".to_owned(), "side@example.test".to_owned())].into();
    assert_eq!(
        named_mailboxes(&placed, &side),
        [("b@example.test".to_owned(), "side@example.test".to_owned())].into()
    );
    let mut both = side.clone();
    both.insert("mb-hub".into(), "hub@example.test".into());
    assert_eq!(
        named_mailboxes(&placed, &both)
            .get("a@example.test")
            .map(String::as_str),
        Some("hub@example.test"),
        "seen once added"
    );
}

/// `settled_in` admits only an owner-private `settled` witness inside a `0700` `alab`
/// directory and refuses symlinks or group/other-readable files.
/// NEGATIVE CONTROL: `std::fs::read_to_string` accepted a `0644` file and a symlink target.
#[test]
fn settled_witness_must_be_owner_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = aterm_tempfile::TempDir::new().unwrap();
    let alab_dir = temp.path().join("alab");
    aterm_types::fs_restricted::ensure_private_dir(&alab_dir).unwrap();
    let table = [0x5a_u8; 32];
    let hex = aterm_codec::hex::encode_lower(&table);
    let settled_path = alab_dir.join("settled");
    aterm_agent::harness::atomic_write::write_atomic_mode(&settled_path, &hex, Some(0o600))
        .unwrap();
    assert!(settled_in(&alab_dir, &table));
    assert!(!settled_in(&alab_dir, &[0x5b_u8; 32]));

    std::fs::set_permissions(&settled_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!settled_in(&alab_dir, &table));

    std::fs::remove_file(&settled_path).unwrap();
    let outside = temp.path().join("outside");
    aterm_agent::harness::atomic_write::write_atomic_mode(&outside, &hex, Some(0o600)).unwrap();
    std::os::unix::fs::symlink(&outside, &settled_path).unwrap();
    assert!(!settled_in(&alab_dir, &table));
}
