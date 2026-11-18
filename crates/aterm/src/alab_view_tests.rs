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

/// One materializer pass over this stand-in home's shared files, as the process that owns the
/// Commons runtime runs one after its stores grow: it projects each file whose key moved.
fn materialize(paths: &Paths, config: &Path) -> Vec<aterm_db::file::materialize::Outcome> {
    aterm_db::file::materialize::pass(
        paths,
        config,
        None,
        &aterm_db::file::materialize::Pass::now(),
    )
}

/// A cache as `install` makes one, at `paths` and `config`: nothing held, nothing read yet.
fn fresh(paths: &Paths, config: &Path) -> &'static Cache {
    Box::leak(Box::new(Cache {
        paths: paths.clone(),
        config: config.to_path_buf(),
        held: Mutex::new(Held {
            projected: None,
            pending: true,
        }),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
    }))
}

/// The lab a cache holds is the one a read of the shared files answers now: a write of this
/// Mac's own that is queued, held, dropped or settled moves no status file and moves what a read
/// lays over the snapshot, so the cache reads again. The naming follow depends on it: once this
/// Mac's held write of its own row gives way (`aterm_db::file::yield_held`), the lab that
/// authorizes the follow must show the row without it (tracker package FL-170).
///
/// NEGATIVE CONTROL, observed: with the cache keyed on the status files alone, the lab held
/// from before the write is served: `left: Some("m7") right: Some("m12")`.
#[test]
fn a_write_of_this_macs_own_moves_the_lab_a_cache_holds() {
    use aterm_db::alab::registry::{MACHINE, MACHINE_FACTS};
    use aterm_db::typed::{cell, keys};
    use aterm_json::json;
    use aterm_secrets::commons::roster::Role;

    let lab = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let machine = keys::machine_pk("m7");
    let owner = |col: &str, value: aterm_json::Value| {
        cell::change(
            &MACHINE,
            cell::CellKey {
                pk: &machine,
                col,
                elem: None,
            },
            &cell::CellValue::owner(MACHINE.version, value),
        )
        .unwrap()
    };
    let cells = vec![
        owner("label", json!("m7")),
        owner("status", json!("active")),
    ];
    let facts = aterm_db::database::Change {
        collection: MACHINE_FACTS.name.into(),
        key: keys::uuid32(&lab.machine(0)),
        value: Some(json!({"r": 1, "n": 1, "mac": machine, "label": "m7"})),
    };
    registry.write(vec![cells, vec![facts]]).unwrap();
    materialize(paths, &config);
    let cache = fresh(paths, &config);
    let label = || {
        alab_roster::Source::snapshot(&cache)
            .lab()
            .and_then(|lab| lab.me.as_ref().and_then(|me| me.label.clone()))
    };
    assert_eq!(label().as_deref(), Some("m7"));
    let status = |paths: &Paths| {
        let root = aterm_db::file::root(paths);
        std::fs::read(root.state(&aterm_db::file::MACHINES.file())).unwrap()
    };
    let before = status(paths);
    let env = aterm_db::file::Env {
        by: "person",
        session: None,
        now_ms: 1,
        attempt: std::time::Duration::ZERO,
        json: false,
        stdin: None,
        editor: &|_, _, _| Err("a rename opens no editor".to_owned()),
        say: &|_| {},
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the test queues a write of this Mac's own row as a rename asked for here does"
    )]
    let queued = aterm_db::file::rename(
        paths,
        &config,
        &machine,
        &[(aterm_db::file::Renamed::Label, "m12")],
        &env,
    );
    queued.expect("queued");
    assert_eq!(status(paths), before, "the write moved no status file");
    assert_eq!(label().as_deref(), Some("m12"));
}

/// SF-04's exit (docs/DESIGN-shared-files-2026-11-11.md §11): the harness's lab is read from
/// the shared files, so it is served across a restart while the registry refuses every strict
/// reader. A stand-in lab's registry holds one Mac (`m7`, bound by this Mac's own facts row),
/// one account and the link between them. Before any pass the source is pending, never absent;
/// after a pass the lab serves the account, the link and this Mac, deciding as the registry's
/// own projection does. The home is then fenced under a writer succession that stays pending
/// (ATERM-KI-1109-1): the registry refuses, a pass records the files `held (succession)`, and a
/// new process (a restart) is served the same lab, with no receipt admitted from it (writers
/// stay strict). With the snapshot gone, a new process is pending, not absent: an empty
/// snapshot never lets a sheet read in a lab's place.
/// NEGATIVE CONTROL: with `Cache::refresh` reading `Registry::observe_installed_projection` as
/// it did before SF-04, the restarted cache is pending: `` a restart is served the held lab ``
/// fails; with the pass marking no machine `=here`, `` this Mac is the machine its own facts
/// row binds `` fails.
#[test]
fn the_lab_is_served_from_the_snapshot_across_a_restart_while_the_registry_refuses() {
    use aterm_db::alab::registry::{ACCOUNT, ADDRESS, MACHINE, MACHINE_FACTS, PREFERENCE};
    use aterm_db::typed::{Collection, cell, keys};
    use aterm_file::{Why, Word};
    use aterm_json::{Value, json};
    use aterm_secrets::commons::roster::Role;

    // Made-up names: no account of the owner's is named in a test.
    const ADA: &str = "ada.fb@alpha.example";
    let lab = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    // The lab's tracker is declared and holds no entry, so its logs' files have a snapshot the
    // fenced pass holds like every other file's.
    aterm_db::file::fixture::declare(paths, &aterm_db::att::TRACKER);
    let ada = keys::address_pk(ADA).unwrap();
    let account = keys::account_pk("claude", ADA).unwrap();
    let machine = keys::machine_pk("m7");
    let link = keys::preference_pk(&machine, &account).unwrap();
    let owner = |collection: &'static Collection, pk: &str, col: &str, value: Value| {
        cell::change(
            collection,
            cell::CellKey {
                pk,
                col,
                elem: None,
            },
            &cell::CellValue::owner(collection.version, value),
        )
        .unwrap()
    };
    let cells = vec![
        owner(&ADDRESS, &ada, "addr", json!(ADA)),
        owner(&MACHINE, &machine, "label", json!("m7")),
        owner(&MACHINE, &machine, "status", json!("active")),
        owner(&ACCOUNT, &account, "provider", json!("anthropic")),
        owner(&ACCOUNT, &account, "realm", json!("claude")),
        owner(&ACCOUNT, &account, "login", json!(ada)),
        owner(&ACCOUNT, &account, "status", json!("active")),
        owner(&PREFERENCE, &link, "machine", json!(machine)),
        owner(&PREFERENCE, &link, "account", json!(account)),
        owner(&PREFERENCE, &link, "rank", json!(1)),
        owner(&PREFERENCE, &link, "why", json!("owner")),
    ];
    let facts = aterm_db::database::Change {
        collection: MACHINE_FACTS.name.into(),
        key: keys::uuid32(&lab.machine(0)),
        value: Some(json!({"r": 1, "n": 1, "mac": machine, "label": "m7"})),
    };
    registry.write(vec![cells, vec![facts]]).unwrap();

    let first = fresh(paths, &config);
    let before = alab_roster::Source::snapshot(&first);
    assert!(
        before.pending() && !before.permits_sheet(),
        "no pass yet: the source is pending, never absent"
    );

    materialize(paths, &config);
    let live = alab_roster::Source::snapshot(&first)
        .lab()
        .expect("a pass's snapshot serves the lab");
    assert_eq!(
        live.me
            .as_ref()
            .map(|me| (me.key.as_str(), me.label.as_deref())),
        Some((machine.as_str(), Some("m7"))),
        "this Mac is the machine its own facts row binds"
    );
    assert_eq!(live.accounts.len(), 1);
    let served = &live.accounts[0];
    assert_eq!(
        (served.pk.as_str(), served.email.as_str(), served.joinable),
        (account.as_str(), ADA, true)
    );
    assert_eq!(
        served.preferred_on(&live.me.as_ref().unwrap().key),
        Some(Some(1)),
        "the link orders this Mac's pool"
    );
    let native = registry.observe_projection(&config).unwrap();
    assert_eq!(
        decided(&live),
        decided(&Cache::lab_for_source(native.source())),
        "the snapshot's lab decides as the registry's own projection does"
    );

    aterm_db::file::fixture::fence_pending(paths, &lab.bootstrap(0));
    let refused = registry
        .observe_projection(&config)
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("still preserving old records"),
        "the registry refuses a strict reader: {refused}"
    );
    for outcome in materialize(paths, &config) {
        assert_eq!(
            (outcome.word, outcome.why),
            (Word::Held, Some(Why::Succession)),
            "{}: {:?}",
            outcome.name,
            outcome.detail
        );
    }

    let restarted = fresh(paths, &config);
    let held = alab_roster::Source::snapshot(&restarted);
    assert!(
        !held.pending() && held.names_this_mac(),
        "a restart is served the held lab"
    );
    assert_eq!(decided(&held.lab().unwrap()), decided(&live));
    assert_eq!(
        decided(&alab_roster::Source::snapshot(&first).lab().unwrap()),
        decided(&live),
        "the running process keeps serving it too"
    );
    let capture = alab_roster::Source::capture(&restarted);
    let store = aterm_agent::harness::accounts::Store::at(&paths.home.join("account-test"));
    let roster = paths.home.join("accounts.toml");
    assert!(
        alab_roster::test_publish_source_roster(
            &store,
            &roster,
            "# refused\n",
            capture.native_guard().unwrap()
        )
        .is_err(),
        "a held lab admits no receipt: writers stay strict"
    );
    assert!(!roster.exists());

    let root = aterm_db::file::root(paths);
    std::fs::remove_file(root.snapshot(&aterm_db::file::ACCOUNTS.file())).unwrap();
    let emptied = alab_roster::Source::snapshot(&fresh(paths, &config));
    assert!(
        emptied.pending() && !emptied.permits_sheet(),
        "a missing snapshot is pending, never absent"
    );
}

pub(super) fn nothing_held() -> Held<Inventory> {
    Held {
        projected: None,
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
    look(&mut held, Ok(Some(naming(Some("m3")))));
    let cache = Cache {
        paths: Paths::for_home(dir.path()),
        config: config.clone(),
        held: Mutex::new(held),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
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
            status: None,
            os: None,
            chip: None,
            ram_gb: None,
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
            status: None,
            os: None,
            chip: None,
            ram_gb: None,
            live: true,
        },
    );
    view.accounts.insert(
        "account-one".into(),
        Account {
            pk: "account-one".into(),
            provider: None,
            realm: Some("claude".into()),
            slug: "claude-one".into(),
            name: None,
            legacy_label: None,
            login: Some("one@example.test".into()),
            identity: None,
            status: Some("active".into()),
            plan: None,
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
        |view: &LabView| harness_lab_with_placed(view, &BTreeMap::new(), &BTreeMap::new());
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

/// The harness lab names every account LabView names, an agy account's included, though its
/// accounts hold only the pooled CLIs': what a surface prints of an account and what a word names
/// (`harness::account_names`) read every realm's names from it. It bars every account whose
/// status lets no sign-in be tried, of every realm (`Lab::barred`): the paused agy account here,
/// which no `LabAccount` carries, and not the active Claude one.
#[test]
fn the_harness_lab_names_every_account_of_every_realm() {
    use aterm_db::{alab::view::Account, typed::keys};

    let email = "ada@x.example";
    let mut view = naming_view(Some("m3"), MachineNickname::Unset);
    for (realm, slug) in [("claude", "claude-ada-x"), ("google", "google-ada-x")] {
        let pk = keys::account_pk(realm, email).unwrap();
        view.accounts.insert(
            pk.clone(),
            Account {
                pk: pk.clone(),
                provider: keys::provider_of_realm(realm).map(str::to_owned),
                realm: Some(realm.into()),
                slug: slug.into(),
                name: keys::account_name(realm, email),
                legacy_label: None,
                login: Some(email.into()),
                identity: None,
                status: Some(
                    if realm == "google" {
                        "paused"
                    } else {
                        "active"
                    }
                    .into(),
                ),
                plan: None,
                region: None,
                flow: None,
                browser: None,
                live: true,
            },
        );
        view.names.insert(
            pk,
            aterm_file::AccountNames {
                realm: realm.into(),
                slug: slug.into(),
                name: keys::account_name(realm, email),
                email: Some(email.into()),
                login: Some(email.into()),
                live: true,
            },
        );
    }
    let lab = harness_lab_with_placed(&view, &BTreeMap::new(), &BTreeMap::new());
    assert_eq!(
        lab.accounts.len(),
        1,
        "Lab.accounts holds only the realms harness_lab_with_placed maps (claude, codex, grok \
         and Kimi's two), so agy's Google account is not among them"
    );
    assert_eq!(lab.names, view.names);
    assert_eq!(
        lab.barred,
        std::iter::once(keys::account_pk("google", email).unwrap()).collect(),
        "the paused agy account is barred, the active Claude one is not"
    );
}

/// A native receipt is decided from the lab's accounts, machines and binding, never from the
/// accounts' names, which no receipt reads: a lab that differs only in a name or a slug (an
/// owner relabelled an account, or added an agy one, and the shared files have not caught up)
/// decides as the same lab, while one whose pooled accounts differ does not.
/// NEGATIVE CONTROL: with `decided` keeping `names`, the relabelled lab decides differently:
/// `` assertion `left == right` failed: a slug alone changes nothing a receipt decided ``.
#[test]
fn a_receipt_is_decided_from_the_labs_accounts_never_their_names() {
    use aterm_db::typed::keys;

    let email = "ada@x.example";
    let pk = keys::account_pk("claude", email).unwrap();
    let names = |slug: &str| aterm_file::AccountNames {
        realm: "claude".into(),
        slug: slug.into(),
        name: keys::account_name("claude", email),
        email: Some(email.into()),
        login: Some(email.into()),
        live: true,
    };
    let lab = Lab {
        names: BTreeMap::from([(pk.clone(), names("claude-ada-x"))]),
        ..Lab::default()
    };
    let mut relabelled = lab.clone();
    relabelled.names.insert(pk.clone(), names("claude-m2-work"));
    let google = keys::account_pk("google", email).unwrap();
    relabelled.names.insert(google, names("google-ada-x"));
    assert_eq!(
        decided(&lab),
        decided(&relabelled),
        "a slug alone changes nothing a receipt decided"
    );
    let pooled = Lab {
        accounts: vec![LabAccount {
            pk: pk.clone(),
            cli: Cli::Claude,
            vendor: Vendor::default(),
            label: keys::reserved_slug("claude", &pk),
            legacy_label: None,
            email: email.into(),
            identity: None,
            browser: None,
            joinable: true,
            preferred: Vec::new(),
        }],
        ..lab.clone()
    };
    assert_ne!(
        decided(&lab),
        decided(&pooled),
        "an account a receipt reads"
    );
}

/// A row a sync adds is named by its account's reserved slug, which never changes and no other
/// account takes, never by its slug, which moves: once the owner relabels Ada's account `m2-work`
/// and Bea's alone derives the short handle Ada's slug was (`claude-fb-alpha`), a Mac whose
/// roster still holds Ada's row under that old slug keeps it, and adds Bea under her own key's
/// label, which no row of Ada's holds.
/// NEGATIVE CONTROL: with `harness_lab_with_placed` labelling an account by its slug (`label:
/// account.slug.clone()`, as it did), Ada's label is the slug her relabel moved: `` left:
/// "claude-m2-work" right: "claude-039f9d73b57117c8" ``.
#[test]
fn a_row_a_sync_adds_is_named_by_a_key_no_other_account_takes() {
    use aterm_agent::harness::{
        accounts::{Roster, Row},
        alab_roster,
    };
    use aterm_db::{
        alab::view::{Account, Preference},
        typed::keys,
    };

    let (ada_email, bea_email) = (
        "ada.fb@alpha.example.invalid",
        "bea.fb@alpha.example.invalid",
    );
    let ada = keys::account_pk("claude", ada_email).unwrap();
    let bea = keys::account_pk("claude", bea_email).unwrap();
    let account = |pk: &str, slug: &str, email: &str| Account {
        pk: pk.into(),
        provider: Some("anthropic".into()),
        realm: Some("claude".into()),
        slug: slug.into(),
        name: None,
        legacy_label: None,
        login: Some(email.into()),
        identity: None,
        status: Some("active".into()),
        plan: None,
        region: None,
        flow: None,
        browser: None,
        live: true,
    };
    let mut view = naming_view(Some("m3"), MachineNickname::Unset);
    view.accounts
        .insert(ada.clone(), account(&ada, "claude-m2-work", ada_email));
    view.accounts
        .insert(bea.clone(), account(&bea, "claude-fb-alpha", bea_email));
    for pk in [&ada, &bea] {
        view.preferences.push(Preference {
            machine: "pk-me".into(),
            account: pk.clone(),
            rank: Some(1),
        });
    }
    let lab = harness_lab_with_placed(&view, &BTreeMap::new(), &BTreeMap::new());
    for account in &lab.accounts {
        assert_eq!(account.label, keys::reserved_slug("claude", &account.pk));
    }
    // Ada's row here, named by the slug her account went by when it was added.
    let existing = Roster {
        rows: vec![Row {
            label: "claude-fb-alpha".into(),
            email: Some(ada_email.into()),
            ..Row::default()
        }],
    };
    let fleet = alab_roster::fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    let label_of = |email: &str| {
        fleet
            .accounts
            .iter()
            .find(|account| account.email == email)
            .map(|account| account.label.clone())
    };
    assert_eq!(label_of(ada_email).as_deref(), Some("claude-fb-alpha"));
    assert_eq!(
        label_of(bea_email),
        Some(keys::reserved_slug("claude", &bea))
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
        let lab = harness_lab(&view, &BTreeMap::new());
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
    look(&mut held, Ok(Some(naming(Some("m86")))));
    let cache = Cache {
        paths: Paths::for_home(root.path()),
        config: config.clone(),
        held: Mutex::new(held),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
    };
    let expected = cache.projection().machine_name();
    assert!(matches!(&expected, RegistryNameRead::Bound(Ok(_))));
    look(
        &mut cache.held.lock().unwrap(),
        Err("temporarily unavailable".into()),
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
        look(&mut cache.held.lock().unwrap(), read);
        assert_eq!(
            cache.projection().machine_name(),
            RegistryNameRead::Pending,
            "an unread first registry is not proof that no binding exists"
        );
        assert!(cache.held.lock().unwrap().pending);
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
        look(&mut held, read);
        let cache = Cache {
            paths: Paths::for_home(root.path()),
            config: root.path().join("aterm.toml"),
            held: Mutex::new(held),
            refreshing: Mutex::new(()),
            mail_refused: Mutex::new(None),
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
    let failed = || Err::<Option<Projected<Inventory>>, String>("custody unreadable".into());
    for (read, what) in [
        (failed(), "a failed read"),
        (Err("source history incomplete".into()), "a partial history"),
    ] {
        let mut held = nothing_held();
        look(&mut held, read);
        assert!(held.pending, "{what}");
        assert!(held.projected.is_none(), "{what}");
    }

    let mut held = nothing_held();
    look(&mut held, Ok(Some(naming(Some("m3")))));
    let lab = held.lab().cloned().expect("the projection is held");
    assert_eq!(
        lab.me.as_ref().and_then(|me| me.label.as_deref()),
        Some("m3")
    );
    assert_eq!(lab.me.as_ref().map(|me| me.key.as_str()), Some("pk-me"));
    assert!(lab.mailboxes.is_empty() && !held.pending);
    assert_eq!(held.projected.as_ref().unwrap().source, Inventory([7; 32]));
    for (read, what) in [
        (Err("source history incomplete".into()), "a partial history"),
        (failed(), "a failed read"),
    ] {
        look(&mut held, read);
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
    Arc::make_mut(&mut next.lab).mailboxes =
        BTreeMap::from([("ada@example.test".to_owned(), "box@example.test".to_owned())]);
    next.source = Inventory([8; 32]);
    look(&mut held, Ok(Some(next)));
    assert!(
        held.lab().is_some_and(|lab| !lab.mailboxes.is_empty()),
        "a complete read installs local overlays with the new source"
    );
    assert_eq!(held.projected.as_ref().unwrap().source, Inventory([8; 32]));
    assert!(!Arc::ptr_eq(held.lab().unwrap(), &lab));
    look(&mut held, Ok(None));
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
        held: Mutex::new(nothing_held()),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
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

/// Write into `registry` a mailbox whose login is `owner@example.test` and the alias
/// `ai@example.test` placed in it; answer the mailbox's key.
fn one_mailbox_and_its_alias(registry: &Registry) -> String {
    use aterm_db::alab::registry::{ADDRESS, MAILBOX};
    use aterm_db::typed::{cell, keys};
    use aterm_json::json;

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
    mailbox
}

/// The lab is read from the shared files a pass wrote, and its mail routes from the registry read
/// beside them.
#[test]
fn native_cache_projects_mail_from_its_captured_source() {
    use aterm_secrets::commons::roster::Role;

    let lab = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let table = registry.open().unwrap();
    let mailbox = one_mailbox_and_its_alias(&registry);
    let config = paths.home.join("aterm.toml");
    let source = registry.observe_projection(&config).unwrap();
    let cache = Cache {
        paths: paths.clone(),
        config,
        held: Mutex::new(Held {
            projected: None,
            pending: true,
        }),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
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
    materialize(paths, &cache.config);
    cache.refresh();
    assert!(
        cache.capture().0.pending(),
        "a cold excluded registry must not become proof of absence"
    );
    aterm_uds::atomic_file::write(&cache.config, b"", 0o600).unwrap();
    materialize(paths, &cache.config);
    cache.refresh();
    let (snapshot, projected) = cache.capture();
    let projected = projected.unwrap();
    assert!(Arc::ptr_eq(&snapshot.lab().unwrap(), &projected.lab));
    let mail = projected.source.mail.as_ref().unwrap();
    assert_eq!(mail.source().revision(), source.source().revision());
    assert_eq!(mail.source().mailboxes()[0].pk, mailbox);
    assert_eq!(
        projected.lab.mailboxes["ai@example.test"],
        "owner@example.test"
    );
}

/// L10's mail half (the accounts upkeep design's law of a run that did not finish): A LAB READ
/// WITHOUT THE MAIL METADATA THE REGISTRY REFUSED IS NEVER KEPT, AND THE REGISTRY'S ANSWER IS USED
/// ONCE IT GIVES ONE, THOUGH NO MARK OF THE SNAPSHOTS MOVED. A stand-in lab's registry holds a
/// mailbox and an alias placed in it, and a pass writes the shared files. The home is then fenced
/// under a writer succession that stays pending (ATERM-KI-1109-1): the registry refuses its mail
/// metadata, and the snapshots still answer. Each read below is made under a trace scope and
/// judged as if a minute later, so no read counts as unsettled:
///
/// - the first serves the lab without its mail routes, looks at the vault, and records the
///   refusal as a retry ([`MAIL_REFUSED`]) and no deadline;
/// - one within a run's first rerun wait (`aterm_uds::trace::RERUN_FIRST`) serves the held lab
///   as it is, records the retry again and no deadline (the backoff's rerun is the one owed),
///   and looks at nothing of the vault;
/// - with the fence taken away (its folder renamed aside) and that wait gone by, the next uses
///   the registry's answer: the alias's mailbox is served, no retry is recorded, and the
///   registry journal's `lstat`s are;
/// - the one after serves that lab, mail routes and all, from what is held, and may be kept.
///
/// NEGATIVE CONTROLS (2026-11-16), each restored and the file touched:
/// - "keep on Err": both `trace::retry(MAIL_REFUSED)` lines in `alab_view.rs` deleted. The
///   verdicts failed, left `[(None, None), (None, None), (None, None), (None, None)]`: the refused
///   read and the one within the wait would be kept from a lab with no mail routes.
/// - In one build, the retry of the read within the wait deleted, and the wait judged through
///   `Since` outside `trace::unrecorded`. The verdicts failed, the second read's
///   `(None, Some(Instant { .. }))` against `(Some(Retry(..)), None)`.
/// - `Cache::mail_refused_lately` answering `false` (no wait). The vault looks failed, left
///   `[true, true, true, false]`, right `[true, false, true, false]`.
#[test]
fn a_lab_read_without_its_refused_mail_metadata_is_never_kept_and_its_answer_is_used_once_given() {
    use aterm_secrets::commons::roster::Role;
    use aterm_uds::trace::{self, Unkept};
    use std::borrow::Cow;
    use std::time::Duration;

    let lab = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    one_mailbox_and_its_alias(&registry);
    materialize(paths, &config);
    aterm_db::file::fixture::fence_pending(paths, &lab.bootstrap(0));
    assert!(
        registry.observe_projection(&config).is_err(),
        "the fenced registry refuses a strict reader"
    );

    let cache = fresh(paths, &config);
    let read = || {
        trace::read_as_if_later(Duration::from_secs(60), || {
            trace::scope(|| alab_roster::Source::snapshot(&cache))
        })
    };
    let routes = |snapshot: &SourceSnapshot| snapshot.lab().map(|lab| lab.mailboxes.clone());
    let in_vault =
        |traced: &trace::Trace| traced.files().any(|(path, _)| path.starts_with(&paths.dir));
    let refusal = Some(Unkept::Retry(Cow::Borrowed(MAIL_REFUSED)));

    let (first, refused) = read();
    let (again, gated) = read();
    // The fence taken away, and the rerun wait since the refusal gone by.
    std::fs::rename(
        paths.dir.join("commons/writer-generation"),
        paths.home.join("fence-aside"),
    )
    .unwrap();
    *cache.mail_refused.lock().unwrap() = Instant::now().checked_sub(trace::RERUN_FIRST);
    let (given, answered) = read();
    let (held, kept) = read();

    let reads = [&refused, &gated, &answered, &kept];
    assert_eq!(
        reads.map(|traced| (traced.unkept(), traced.mono_deadline())),
        [
            (refusal.clone(), None),
            (refusal.clone(), None),
            (None, None),
            (None, None)
        ],
        "a read that served the lab without its refused mail metadata is not kept, and records \
         no deadline before the backoff's rerun; one that served it with them may be kept"
    );
    let none = Some(BTreeMap::new());
    let mail = Some(BTreeMap::from([(
        "ai@example.test".to_owned(),
        "owner@example.test".to_owned(),
    )]));
    assert_eq!(
        [&first, &again, &given, &held].map(routes),
        [none.clone(), none, mail.clone(), mail],
        "the lab is served without its mail routes while refused, and with them once answered"
    );
    assert_eq!(
        reads.map(in_vault),
        [true, false, true, false],
        "within the rerun wait, and once its answer is held, the registry is not read again"
    );
    assert!(
        answered
            .files()
            .any(|(path, _)| path.starts_with(&paths.dir)
                && path.extension().is_some_and(|suffix| suffix == "astream")),
        "an answered read records the registry journal's lstats"
    );
}

#[test]
fn a_new_installation_is_an_absent_source_without_custody() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let cache: &'static Cache = Box::leak(Box::new(Cache {
        paths: paths.clone(),
        config: paths.home.join("aterm.toml"),
        held: Mutex::new(Held {
            projected: None,
            pending: true,
        }),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
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
        held: Mutex::new(Held {
            projected: None,
            pending: true,
        }),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
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
    use aterm_db::alab::registry::{ACCOUNT, ADDRESS};
    use aterm_db::typed::{Collection, cell, keys};
    use aterm_secrets::commons::roster::Role;

    for change in 0..8 {
        let fixture = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
        let paths = &fixture.paths[0];
        let registry = Registry::at(paths.clone());
        let table = registry.open().unwrap();
        let config = paths.home.join("aterm.toml");
        aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
        if change == 5 {
            fixture.rotate(&[Role::Reader]);
        }
        let cache: &'static Cache = Box::leak(Box::new(Cache {
            paths: paths.clone(),
            config: config.clone(),
            held: Mutex::new(Held {
                projected: None,
                pending: true,
            }),
            refreshing: Mutex::new(()),
            mail_refused: Mutex::new(None),
        }));
        materialize(paths, &config);
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
                    decided(&Cache::lab_for_source(
                        registry.observe_projection(&config).unwrap().source()
                    )),
                    decided(&old_lab)
                );
                if change == 4 {
                    cache.refresh();
                }
            }
            2 => {
                aterm_uds::atomic_file::write(&config, b"[alab]\nroster = false\n", 0o600).unwrap()
            }
            3 => {
                // Made up: an account the lab lists once this write lands.
                const BEA: &str = "bea@example.test";
                let address = keys::address_pk(BEA).unwrap();
                let account = keys::account_pk("claude", BEA).unwrap();
                let owner = |collection: &'static Collection,
                             pk: &str,
                             col: &str,
                             value: aterm_json::Value| {
                    cell::change(
                        collection,
                        cell::CellKey {
                            pk,
                            col,
                            elem: None,
                        },
                        &cell::CellValue::owner(collection.version, value),
                    )
                    .unwrap()
                };
                registry
                    .write(vec![vec![
                        owner(&ADDRESS, &address, "addr", aterm_json::json!(BEA)),
                        owner(
                            &ACCOUNT,
                            &account,
                            "provider",
                            aterm_json::json!("anthropic"),
                        ),
                        owner(&ACCOUNT, &account, "realm", aterm_json::json!("claude")),
                        owner(&ACCOUNT, &account, "login", aterm_json::json!(address)),
                        owner(&ACCOUNT, &account, "status", aterm_json::json!("active")),
                    ]])
                    .unwrap();
                assert_ne!(
                    decided(&Cache::lab_for_source(
                        registry.observe_projection(&config).unwrap().source()
                    )),
                    decided(&old_lab),
                    "the lab lists the account the write made"
                );
            }
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
        // A write that leaves the lab as it was (1, and 4 read again) decides nothing
        // differently: the receipt is admitted. A changed policy (2), a write that changes the
        // lab (3), or a registry the admission cannot read whole (6), admits none.
        let admitted = matches!(change, 0 | 1 | 4 | 5 | 7);
        assert_eq!(result.is_ok(), admitted, "change={change}: {result:?}");
        assert_eq!(roster.exists(), admitted);
        assert_eq!(store.dir.join("sync-commit.json").exists(), admitted);
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
        held: Mutex::new(Held {
            projected: None,
            pending: false,
        }),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
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
        held: Mutex::new(nothing_held()),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
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
