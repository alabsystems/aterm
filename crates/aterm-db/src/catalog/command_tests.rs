// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Where in [`readiness`] a hook runs: before each read of this Mac's vault that a vault
/// writer can hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Point {
    /// Before the custody is revalidated.
    Custody,
    /// Before the log-backed roster is read.
    Roster,
    /// Before this Mac's vault is asked whether it denies this Mac.
    Denial,
    /// Before each read of this home's access.
    Access,
}

/// A hook and the point it waits for; `None` waits for whichever point comes next.
type Hook = (Option<Point>, Box<dyn FnOnce()>);

thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

/// Run `hook` once this thread's next readiness read reaches `point`, or whichever point it
/// reaches next where `point` is `None`. A hook may set the next hook.
fn at(point: Option<Point>, hook: impl FnOnce() + 'static) {
    HOOK.with(|slot| *slot.borrow_mut() = Some((point, Box::new(hook))));
}

/// Drop this thread's hook, run or not.
fn clear() {
    HOOK.with(|slot| slot.borrow_mut().take());
}

pub(super) fn reached(point: Point) {
    let hook = HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|(at, _)| at.is_none_or(|at| at == point))
        {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, hook)) = hook {
        hook();
    }
}

/// A vault writer that takes this Mac's state lock while readiness reads makes the read busy
/// (`control-busy`), which a reader observes again, never a verdict it did not judge, at each
/// read it can hold: the custody's revalidation, the log-backed roster, the denial and the
/// access. A home whose membership is the control journal's and one whose membership is
/// log-backed (F18) are each read at every point their read reaches, the lock let go at the
/// read's next point so each row judges the one read it held; with no lock, each is ready.
/// NEGATIVE CONTROL (2026-11-08): with readiness asking `local_recipient_denied_in_roster`,
/// `Bootstrap::access_in` and `revalidate_custody` with `?` again, and a roster read meeting a
/// vault writer answering a plain error, every row fails; with the revalidation's answer
/// ignored and a roster or denial read meeting a writer taken as none, every custody, roster
/// and denial row reads `ready`.
#[test]
fn a_vault_writer_during_readiness_is_busy_not_a_membership_verdict() {
    use aterm_secrets::{client, commons::roster::Role};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    let journal = crate::table::tests::Lab::new(&[Role::Writer]);
    let logged = crate::table::tests::Lab::with_foundation(&[Role::Writer, Role::Writer]);
    client::seed_vault_genesis(&logged.paths[0], &logged._home.path().join("genesis.v2")).unwrap();
    let homes: [(&str, &crate::table::tests::Lab, &[Point]); 2] = [
        (
            "journal",
            &journal,
            &[Point::Custody, Point::Denial, Point::Access],
        ),
        (
            "log-backed",
            &logged,
            &[Point::Custody, Point::Roster, Point::Denial, Point::Access],
        ),
    ];
    let hang_detector = || std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let state = |read: Result<Value>| read.map(|value| value["state"].clone());
    let mut outcomes = Vec::new();
    for (home, lab, points) in homes {
        let paths = &lab.paths[0];
        let bootstrap = lab.bootstrap(0);
        let log_backed = client::trusted_commons_log(paths, bootstrap.trust())
            .unwrap()
            .is_some();
        let config = paths.home.join("config.toml");
        for &point in points {
            let held = Rc::new(RefCell::new(None));
            let taken = Rc::new(Cell::new(false));
            at(Some(point), {
                let (held, taken, paths) = (held.clone(), taken.clone(), paths.clone());
                move || {
                    *held.borrow_mut() = Some(client::test_hold_state_lock(&paths).unwrap());
                    taken.set(true);
                    // Let go at the read's next point, so no later read's busy answer can
                    // stand in for this one's.
                    at(None, move || drop(held.borrow_mut().take()));
                }
            });
            let busy = state(readiness(paths, &config, hang_detector()));
            drop(held.borrow_mut().take());
            clear();
            let ready = state(readiness(paths, &config, hang_detector()));
            outcomes.push((home, point, log_backed, taken.get(), busy, ready));
        }
    }
    let failed: Vec<_> = outcomes
        .iter()
        .filter(|(home, _, log_backed, held, busy, ready)| {
            *log_backed != (*home == "log-backed")
                || !*held
                || busy.as_ref().ok() != Some(&json!("control-busy"))
                || ready.as_ref().ok() != Some(&json!("ready"))
        })
        .collect();
    assert!(
        failed.is_empty(),
        "(home, point, log-backed, held, read while held, read after): {failed:?}"
    );
}

/// A vault writer that finishes between readiness's log read and its roster read (here a
/// removal of the other member) leaves the log readiness read behind the history its roster
/// read requires. The log moved, which readiness reports as busy (`control-busy`) and the next
/// read clears, never a failed command.
/// NEGATIVE CONTROL (2026-11-08): with the roster read's error returned whether or not the log
/// moved, the raced read fails ("the acknowledged membership history is unavailable").
#[test]
fn a_vault_write_finished_between_the_log_and_roster_reads_is_busy() {
    use aterm_secrets::{client, commons::roster::Role};
    use std::{cell::Cell, rc::Rc};

    let lab = crate::table::tests::Lab::with_foundation(&[Role::Writer, Role::Writer]);
    client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    let other = lab.bootstrap(1).request().unwrap().binding().actor();
    let paths = lab.paths[0].clone();
    let config = paths.home.join("config.toml");
    let hang_detector = || std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let state = |read: Result<Value>| read.map(|value| value["state"].clone());
    let removed = Rc::new(Cell::new(0));
    at(Some(Point::Roster), {
        let (paths, removed) = (paths.clone(), removed.clone());
        move || removed.set(client::remove_member_in_log(&paths, other).unwrap().len())
    });
    let raced = state(readiness(&paths, &config, hang_detector()));
    clear();
    let next = state(readiness(&paths, &config, hang_detector()));
    assert!(
        removed.get() > 0
            && raced.as_ref().ok() == Some(&json!("control-busy"))
            && next.as_ref().ok() == Some(&json!("ready")),
        "records removed: {}, raced read: {raced:?}, next read: {next:?}",
        removed.get()
    );
}

/// A Mac whose vault names Commons but which never opened it (no device key, custody or
/// control history) reads `custody-pending` and still holds no custody after the read, and an
/// established reader (`aterm db commons-tables`) says so in plain words rather than naming a
/// missing directory.
/// NEGATIVE CONTROL (2026-11-08): with readiness and `established_with_journal_until` not
/// asking `custody_opened`, `commons-status` failed ("the directory …/commons: there is no
/// such directory").
#[test]
fn a_mac_that_never_opened_commons_reads_custody_pending_and_creates_nothing() {
    use aterm_secrets::commons::roster::Role;

    let lab = crate::table::tests::Lab::with_foundation_enrolled(&[Role::Writer, Role::Writer], 1);
    let paths = &lab.paths[1];
    let before = crate::tests::files(lab._home.path());
    let hang_detector = || std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    assert!(matches!(
        aterm_secrets::commons::observe_until(paths, hang_detector()),
        Ok(aterm_secrets::commons::Observation::Visible(_))
    ));
    let status = commons_status_until(paths, &paths.home.join("config.toml"), hang_detector())
        .map(|value| (value["state"].clone(), value["membership_checked"].clone()));
    let established = Bootstrap::established_with_journal_until(paths, hang_detector())
        .map(|_| ())
        .map_err(|error| error.to_string());
    let created = crate::tests::files(lab._home.path()) != before;
    assert!(
        status.as_ref().ok() == Some(&(json!("custody-pending"), json!(false)))
            && established
                .as_ref()
                .is_err_and(|error| error.starts_with("this Mac has opened no Commons custody yet"))
            && !created,
        "status: {status:?}, established: {established:?}, files created: {created}"
    );
}

#[test]
fn authority_inspection_never_initializes_an_absent_control_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("owner"));
    let config = root.path().join("config");
    let read = || run_at("commons-authority", &mut BTreeMap::new(), &paths, &config).unwrap();
    assert_eq!(read()["authority_available"].as_bool(), Some(false));
    assert!(!paths.home.exists());
    let source = root.path().join("vault");
    aterm_secrets::client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1"))
        .unwrap();
    let before = std::fs::read(&source).unwrap();
    let observed = read();
    assert_eq!(observed["authority_available"].as_bool(), Some(true));
    assert_eq!(observed["control"].as_str(), Some("absent"));
    assert!(observed["published_audience"].is_null());
    assert_eq!(observed["can_initialize_network"].as_bool(), Some(false));
    assert!(!paths.dir.join("commons").exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
}

#[test]
fn authority_inspection_requires_cached_admin_and_reports_only_public_metadata() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Reader]);
    let read = |index: usize| {
        run_at(
            "commons-authority",
            &mut BTreeMap::new(),
            &lab.paths[index],
            &lab.paths[index].home.join("config"),
        )
        .unwrap()
    };
    let owner = read(0);
    assert_eq!(owner["control"].as_str(), Some("typed-descriptor"));
    assert_eq!(owner["descriptor"]["authority"], owner["authority"]);
    assert_eq!(
        owner["published_audience"],
        json!({ "kind": "hosts", "hosts": ["m1", "m2"] })
    );
    assert_eq!(owner["can_initialize_network"].as_bool(), Some(false));
    let member = read(1);
    assert_eq!(member["authority_available"].as_bool(), Some(false));
    assert!(member.get("authority").is_none());
    assert!(member.get("descriptor").is_none());
    assert!(member.get("published_audience").is_none());
    let invalid = run_at(
        "commons-authority",
        &mut BTreeMap::from([("--application", "ignored")]),
        &lab.paths[0],
        &lab.paths[0].home.join("config"),
    );
    assert!(invalid.is_err());
}

#[test]
fn authority_inspection_remains_available_when_writer_selection_is_unreadable() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    let read = || run_at("commons-authority", &mut BTreeMap::new(), paths, &config).unwrap();
    let before = read();
    assert_eq!(before["authority_available"], json!(true));
    let recovery = paths.dir.join("commons/writer-generation");
    aterm_secrets::fsx::ensure_private_dir(&recovery).unwrap();
    let selection = recovery.join("selection");
    aterm_secrets::fsx::write_new(&selection, b"incomplete selection", 0o600).unwrap();
    assert!(crate::generation::selected(paths).is_err());
    assert_eq!(read(), before);
    assert_eq!(std::fs::read(selection).unwrap(), b"incomplete selection");
}

fn status(paths: &Paths) -> Value {
    run_at(
        "commons-status",
        &mut BTreeMap::new(),
        paths,
        &paths.home.join("config"),
    )
    .unwrap()
}

fn inspect_inventory(paths: &Paths, config: &Path, table: Digest) -> Result<Value> {
    let encoded = aterm_codec::hex::encode_lower(&table);
    run_at(
        "commons-inventory",
        &mut BTreeMap::from([("--table", encoded.as_str())]),
        paths,
        config,
    )
}

#[test]
fn inspections_preserve_missing_current_and_ancestor_custody() {
    use aterm_secrets::commons::roster::Role;

    fn read(command: &str, paths: &Paths, config: &Path, table: Digest) -> Result<Value> {
        let table = aterm_codec::hex::encode_lower(&table);
        let mut options = if matches!(command, "commons-inventory" | "commons-custody") {
            BTreeMap::from([("--table", table.as_str())])
        } else {
            BTreeMap::new()
        };
        run_at(command, &mut options, paths, config)
    }

    let mut violations = Vec::new();
    for recovered in [false, true] {
        for command in [
            "commons-inventory",
            "commons-tables",
            "commons-status",
            "commons-custody",
            "commons-authority",
        ] {
            for name in ["commons.lock", "commons.claim"] {
                let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
                let paths = &lab.paths[0];
                let config = paths.home.join("config.toml");
                let table = {
                    let bootstrap = lab.bootstrap(0);
                    let session = Session::open(&bootstrap).unwrap();
                    let directory = Directory::new(paths, bootstrap.trust());
                    let descriptor =
                        Descriptor::application(bootstrap.trust(), "inspection", "rows", 1)
                            .unwrap();
                    let scope = directory.publish(&session, &descriptor).unwrap();
                    directory
                        .open_store(&session, scope)
                        .unwrap()
                        .commit_catalog(&session, b"private inspection fixture")
                        .unwrap();
                    scope.digest()
                };
                if recovered {
                    crate::generation::test_complete_successor(
                        lab._home.path(),
                        paths,
                        &lab.paths[1],
                        &config,
                        std::time::Instant::now() + std::time::Duration::from_secs(60),
                    )
                    .unwrap();
                }
                read(command, paths, &config, table).unwrap();
                let path = paths.dir.join(name);
                std::fs::rename(&path, path.with_extension("preserved")).unwrap();
                let before = crate::tests::files(lab._home.path());
                let result = read(command, paths, &config, table);
                let changed = crate::tests::files(lab._home.path()) != before;
                let accepted = match command {
                    "commons-authority" => false,
                    "commons-status" => result
                        .as_ref()
                        .is_ok_and(|value| value["ready"].as_bool() == Some(true)),
                    _ => result.is_ok(),
                };
                if changed || accepted {
                    violations.push((recovered, command, name, changed, accepted));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "(successor, command, file, changed, accepted): {violations:?}"
    );
}

#[test]
fn status_preserves_an_unconfigured_home() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("unconfigured"));
    let value = run_at(
        "commons-status",
        &mut BTreeMap::new(),
        &paths,
        &paths.home.join("config.toml"),
    )
    .unwrap();
    assert_eq!(value["ready"].as_bool(), Some(false));
    assert_eq!(value["can_initialize_network"].as_bool(), Some(false));
    assert_eq!(value["membership_history"]["state"], json!("unavailable"));
    assert!(value["repairs"]["peers"].is_array());
    assert!(!paths.home.exists());
}

#[test]
fn membership_history_is_reported_without_admin_or_table_initialization() {
    use aterm_secrets::{client, commons::roster::Role};
    let lab = crate::table::tests::Lab::with_foundation(&[Role::Writer, Role::Reader]);
    client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    let encoded = client::load_records(&lab.paths[0]).unwrap().encode();
    client::save_records(&lab.paths[1], &encoded).unwrap();
    lab.bootstrap(1);
    assert!(client::admin_cached(&lab.paths[1]).is_err());
    let store = client::load_records(&lab.paths[1]).unwrap();
    let before = crate::tests::files(lab._home.path());
    let value = status(&lab.paths[1]);
    let observed = &value["membership_history"];
    assert_eq!(observed["state"], json!("available"));
    assert_eq!(observed["retained_records"], json!(store.len()));
    assert_eq!(
        observed["complete_records"],
        json!(store.log().unwrap().held_count())
    );
    assert_eq!(
        observed["limits"]["held_records"],
        json!(store.log().unwrap().budgets().held)
    );
    assert_eq!(
        observed["remaining_records"],
        json!(store.log().unwrap().budgets().held - store.len())
    );
    assert_eq!(value["can_initialize_network"], json!(false));
    assert_eq!(crate::tests::files(lab._home.path()), before);
    assert!(!lab.paths[1].dir.join("commons/tables").exists());
}

/// A Mac that opened Commons and then lost its `commons` folder but kept its claim (a partial
/// reset) holds custody to recover, not custody pending: `commons-status` fails naming the
/// recovery rather than the missing folder.
/// NEGATIVE CONTROL (2026-11-08): without the established open's claim check, the read failed
/// ("the directory …/commons: there is no such directory").
#[test]
fn a_kept_claim_without_its_commons_folder_is_custody_to_recover_not_pending() {
    use aterm_secrets::commons::roster::Role;

    let lab = crate::table::tests::Lab::with_foundation(&[Role::Writer]);
    let paths = &lab.paths[0];
    std::fs::rename(
        paths.dir.join("commons"),
        lab._home.path().join("commons-set-aside"),
    )
    .unwrap();
    let hang_detector = || std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let status = commons_status_until(paths, &paths.home.join("config.toml"), hang_detector())
        .map(|value| value["state"].clone())
        .map_err(|error| error.to_string());
    assert_eq!(
        status,
        Err("established Commons directory is missing; recovery is required".into())
    );
}

/// A Mac with a device key, whose vault names Commons, that has opened no custody reads
/// `custody-pending` (a device key alone is no custody), and the read creates nothing.
#[test]
fn status_does_not_establish_visible_but_uninitialized_custody() {
    let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let paths = &lab.paths[0];
    let before = crate::tests::files(lab._home.path());
    let state = run_at(
        "commons-status",
        &mut BTreeMap::new(),
        paths,
        &paths.home.join("config"),
    )
    .map(|value| value["state"].clone());
    assert_eq!(state.ok(), Some(json!("custody-pending")));
    assert!(crate::tests::files(lab._home.path()) == before);
}

#[test]
fn inventory_keeps_current_and_ancestor_vault_sources_through_its_read() {
    use aterm_secrets::commons::roster::Role;
    use aterm_uds::private_file;
    use std::time::Instant;

    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let original = &lab.paths[0];
    let config = original.home.join("config");
    let bootstrap = lab.bootstrap(0);
    let (scope, receipt) = {
        let session = Session::open(&bootstrap).unwrap();
        let directory = Directory::new(original, bootstrap.trust());
        let scope = directory
            .open_application(&session, "inventory-source-read", "rows", 1)
            .unwrap();
        let receipt = directory
            .open_store(&session, scope)
            .unwrap()
            .commit_catalog(&session, b"private inventory boundary fixture")
            .unwrap();
        (scope, receipt)
    };
    let selected = crate::generation::test_complete_successor(
        lab._home.path(),
        original,
        &lab.paths[1],
        &config,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let directory = Directory::new(&selected, bootstrap.trust());
    let data = directory.path(scope).unwrap().join("table.astream");
    let before = std::fs::read(&data).unwrap();
    let writers = [original, &selected]
        .map(|paths| private_file::open_private(&paths.dir, ".lock").unwrap().0);
    let mut attempts = 0;
    let mut entered = Vec::new();
    let read = retained_inventory_admitted(
        &selected,
        &config,
        scope.digest(),
        None,
        crate::admission::deadline(),
        || {
            // A real vault writer tries to enter after session admission, at
            // the gap that previously let an inventory read fail mid-query.
            for writer in writers {
                attempts += 1;
                #[expect(
                    clippy::disallowed_methods,
                    reason = "inject an exclusive vault writer at the inventory admission boundary"
                )]
                match writer.try_lock() {
                    Ok(()) => entered.push(writer),
                    Err(std::fs::TryLockError::WouldBlock) => {}
                    Err(error) => panic!("vault writer attempt failed: {error:?}"),
                }
            }
        },
    )
    .expect("admitted inventory keeps its vault sources through the whole read");
    assert_eq!(attempts, 2);
    assert!(entered.is_empty());
    assert_eq!(read["known"]["count"], json!(1));
    assert_eq!(read["eligible"], read["known"]);
    let known = crate::replica::InventorySummary::of(&BTreeSet::from([receipt.id]));
    assert_eq!(
        read["known"]["sha256"],
        json!(aterm_codec::hex::encode_lower(&known.fingerprint))
    );
    assert!(
        !read
            .to_string()
            .contains("private inventory boundary fixture")
    );
    assert_eq!(std::fs::read(&data).unwrap(), before);

    for paths in [original, &selected] {
        let writer = private_file::open_private(&paths.dir, ".lock").unwrap().0;
        #[expect(
            clippy::disallowed_methods,
            reason = "the completed inventory must release every vault source before returning"
        )]
        writer
            .try_lock()
            .expect("inventory source guard was released");
        writer.unlock().unwrap();
    }
    aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    assert!(inspect_inventory(original, &config, scope.digest()).is_err());
    assert_eq!(std::fs::read(data).unwrap(), before);
}

#[test]
fn recovery_inventory_and_table_listing_identify_receive_only_storage() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let scope = directory
        .open_application(&session, "recovery-inspection", "rows", 1)
        .unwrap();
    let catalog_scope = directory.catalog_scope();
    let catalog = directory.open_store(&session, catalog_scope).unwrap();
    let declaration = catalog
        .inventory(&session)
        .unwrap()
        .next()
        .copied()
        .unwrap();
    let catalog_wire = catalog
        .wire(&session, &declaration)
        .unwrap()
        .unwrap()
        .to_vec();
    drop(catalog);
    let mut store = directory.open_store(&session, scope).unwrap();
    let record = store
        .commit_catalog(&session, b"private-recovered-value")
        .unwrap();
    let wire = store.wire(&session, &record.id).unwrap().unwrap().to_vec();
    drop(store);
    for (id, name) in [
        (catalog_scope, "original-catalog"),
        (scope, "original-table"),
    ] {
        std::fs::rename(directory.path(id).unwrap(), lab._home.path().join(name)).unwrap();
    }
    let recovery = directory.recovery();
    recovery
        .catalog(&session)
        .unwrap()
        .ingest(&session, &catalog_wire)
        .unwrap();
    recovery
        .enroll(&session, scope)
        .unwrap()
        .ingest(&session, &wire)
        .unwrap();
    drop(session);
    let config = paths.home.join("config");
    let read = inspect_inventory(paths, &config, scope.digest()).unwrap();
    assert_eq!(read["recovery_reader"], json!(true));
    assert_eq!(read["writer_resume_authorized"], json!(false));
    assert_eq!(read["known"]["count"], json!(1));
    assert!(!read.to_string().contains("private-recovered-value"));
    let listing = run_at("commons-tables", &mut BTreeMap::new(), paths, &config).unwrap();
    assert_eq!(listing["recovery_reader"], json!(true));
    assert_eq!(listing["tables"].as_array().unwrap().len(), 1);
    aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    let disabled = inspect_inventory(paths, &config, scope.digest()).unwrap();
    assert_eq!(disabled["storage"], json!("not-observed"));
    assert_eq!(disabled["known"], Value::Null);
    assert!(!directory.path(catalog_scope).unwrap().exists());
}

#[test]
fn retained_inventory_rejects_arguments_before_opening_custody() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("absent"));
    let valid = aterm_codec::hex::encode_lower(&[9; 32]);
    for command in ["commons-inventory", "commons-custody"] {
        for mut options in [
            BTreeMap::new(),
            BTreeMap::from([("--table", "bad")]),
            BTreeMap::from([("--table", valid.as_str()), ("--unexpected", "value")]),
        ] {
            assert!(run_at(command, &mut options, &paths, &paths.home.join("config"),).is_err());
            assert!(!paths.home.exists());
        }
    }
    for fields in [
        vec![("--records", "0")],
        vec![("--records", "257")],
        vec![("--records", "+1")],
        vec![("--records", "999999999999999999999999999999")],
        vec![("--after", valid.as_str())],
        vec![("--records", "2"), ("--after", "bad")],
    ] {
        let mut options = BTreeMap::from([("--table", valid.as_str())]);
        options.extend(fields);
        assert!(
            run_at(
                "commons-inventory",
                &mut options,
                &paths,
                &paths.home.join("config")
            )
            .is_err()
        );
        assert!(!paths.home.exists());
    }
    let duplicate = format!("{valid},{valid}");
    let too_many = (0..65u8)
        .map(|n| aterm_codec::hex::encode_lower(&[n; 32]))
        .collect::<Vec<_>>()
        .join(",");
    for mut options in [
        BTreeMap::from([("--tables", "")]),
        BTreeMap::from([("--tables", duplicate.as_str())]),
        BTreeMap::from([("--tables", too_many.as_str())]),
        BTreeMap::from([("--tables", valid.as_str()), ("--table", valid.as_str())]),
        BTreeMap::from([("--tables", valid.as_str()), ("--records", "1")]),
    ] {
        assert!(
            run_at(
                "commons-inventory",
                &mut options,
                &paths,
                &paths.home.join("config")
            )
            .is_err()
        );
        assert!(!paths.home.exists());
    }
}

#[test]
fn inventory_batch_shares_custody_and_keeps_each_table_outcome() {
    use aterm_secrets::commons::roster::Role;
    use aterm_uds::private_file::PrivateLock;

    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let descriptors: Vec<_> = ["present", "missing", "excluded", "damaged"]
        .into_iter()
        .map(|role| Descriptor::application(bootstrap.trust(), "batch-inventory", role, 1).unwrap())
        .collect();
    {
        let mut catalog = directory.catalog(&session).unwrap();
        for descriptor in &descriptors {
            catalog
                .commit_catalog(&session, &descriptor.encode().unwrap())
                .unwrap();
        }
    }
    let receipt = directory
        .enroll(&session, descriptors[0].scope)
        .unwrap()
        .commit_catalog(&session, b"private batch inventory sentinel")
        .unwrap();
    for descriptor in &descriptors[2..] {
        drop(directory.enroll(&session, descriptor.scope).unwrap());
        std::fs::write(
            directory
                .path(descriptor.scope)
                .unwrap()
                .join("table.anchor"),
            b"damaged",
        )
        .unwrap();
    }
    aterm_uds::atomic_file::write(
        &config,
        format!(
            "[commons]\nexclude_tables = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(&descriptors[2].scope.digest())
        )
        .as_bytes(),
        0o600,
    )
    .unwrap();
    let now = aterm_time::unix_millis().unwrap() / 1000;
    let ring_scope = ring::CLAIMS
        .scope(bootstrap.trust(), now / ring::CLAIMS.period_s)
        .unwrap();
    let tables: Vec<_> = descriptors
        .iter()
        .map(|d| d.scope.digest())
        .chain([
            directory.catalog_scope().digest(),
            ring_scope.digest(),
            [9; 32],
        ])
        .collect();
    let before = crate::tests::files(lab._home.path());
    let held = std::cell::RefCell::new(None);
    let results = retained_inventories_admitted(
        paths,
        &config,
        &tables,
        None,
        crate::admission::deadline(),
        || {
            *held.borrow_mut() = Some(
                PrivateLock::take(
                    &paths.dir,
                    "commons.lock",
                    aterm_spec::wait::MUST_HAPPEN,
                    "test custody held",
                )
                .unwrap(),
            );
        },
    )
    .unwrap();
    // Reopening bootstrap for a later table would contend with this held lock.
    assert!(
        PrivateLock::take(
            &paths.dir,
            "commons.lock",
            std::time::Duration::ZERO,
            "test custody held"
        )
        .is_err()
    );
    assert_eq!(results.len(), tables.len());
    let present = results[0].as_ref().unwrap();
    assert_eq!(present["storage"], json!("present"));
    assert_eq!(present["known"]["count"], json!(1));
    let expected = crate::replica::InventorySummary::of(&BTreeSet::from([receipt.id]));
    assert_eq!(
        present["known"]["sha256"],
        json!(aterm_codec::hex::encode_lower(&expected.fingerprint))
    );
    assert_eq!(results[1].as_ref().unwrap()["storage"], json!("missing"));
    assert_eq!(
        results[2].as_ref().unwrap()["subscription"],
        json!("excluded")
    );
    assert_eq!(
        results[2].as_ref().unwrap()["storage"],
        json!("not-observed")
    );
    assert!(results[3].is_err());
    assert_eq!(results[4].as_ref().unwrap()["known"]["count"], json!(4));
    assert_eq!(results[5].as_ref().unwrap()["storage"], json!("missing"));
    assert_eq!(
        results[6].as_ref().unwrap()["subscription"],
        json!("undeclared")
    );
    drop(held.into_inner());

    let healthy = tables
        .iter()
        .filter(|id| **id != descriptors[3].scope.digest())
        .map(|id| aterm_codec::hex::encode_lower(id))
        .collect::<Vec<_>>()
        .join(",");
    let healthy = run_at(
        "commons-inventory",
        &mut BTreeMap::from([("--tables", healthy.as_str())]),
        paths,
        &config,
    )
    .unwrap();
    assert_eq!(healthy["ok"], json!(true));
    assert_eq!(
        healthy["tables"].as_array().unwrap().len(),
        tables.len() - 1
    );

    let request = tables
        .iter()
        .map(|id| aterm_codec::hex::encode_lower(id))
        .collect::<Vec<_>>()
        .join(",");
    let response = run_at(
        "commons-inventory",
        &mut BTreeMap::from([("--tables", request.as_str())]),
        paths,
        &config,
    )
    .unwrap();
    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["atomic"], json!(false));
    assert_eq!(response["tables"][0]["known"], present["known"]);
    assert_eq!(response["tables"][3]["storage"], json!("unavailable"));
    assert!(response["tables"][3]["known"].is_null());
    assert!(response["tables"][3]["error"].as_str().is_some());
    assert!(
        !response
            .to_string()
            .contains("private batch inventory sentinel")
    );
    assert_eq!(crate::tests::files(lab._home.path()), before);
    assert!(!directory.path(descriptors[1].scope).unwrap().exists());
}

#[test]
fn inventory_pages_cover_retained_records_without_values_or_writes() {
    use aterm_secrets::commons::roster::Role;

    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let descriptor =
        Descriptor::application(bootstrap.trust(), "inventory-pages", "rows", 1).unwrap();
    let scope = directory.publish(&session, &descriptor).unwrap();
    let table_path = directory.path(scope).unwrap();
    let mut store = Store::open(&session, &table_path, scope).unwrap();
    let mut expected = BTreeSet::new();
    for index in 0..5 {
        let value = format!("private paged inventory payload {index}");
        expected.insert(store.commit_catalog(&session, value.as_bytes()).unwrap().id);
    }
    let before = std::fs::read(table_path.join("table.astream")).unwrap();
    let table = aterm_codec::hex::encode_lower(&scope.digest());
    let mut after = None;
    let mut seen = Vec::new();
    for count in [2, 2, 1] {
        let mut options = BTreeMap::from([("--table", table.as_str()), ("--records", "2")]);
        if let Some(after) = after.as_deref() {
            options.insert("--after", after);
        }
        let value = run_at("commons-inventory", &mut options, paths, &config).unwrap();
        assert_eq!(value["known"]["count"], json!(5));
        assert_eq!(value["known"], value["eligible"]);
        let page = &value["page"];
        assert_eq!(page["after"], json!(after));
        let records = page["records"].as_array().unwrap();
        assert_eq!(records.len(), count);
        for record in records {
            assert_eq!(record["eligible"], json!(true));
            let id = inspection_digest(record["operation"].as_str().unwrap(), "operation").unwrap();
            assert!(seen.last().is_none_or(|previous| *previous < id));
            seen.push(id);
        }
        after = page["next_after"].as_str().map(str::to_owned);
        assert_eq!(after.is_some(), count == 2);
        assert!(
            !value
                .to_string()
                .contains("private paged inventory payload")
        );
        assert_eq!(
            std::fs::read(table_path.join("table.astream")).unwrap(),
            before
        );
    }
    assert_eq!(seen.into_iter().collect::<BTreeSet<_>>(), expected);

    let last = aterm_codec::hex::encode_lower(expected.last().unwrap());
    let mut options = BTreeMap::from([
        ("--table", table.as_str()),
        ("--records", "2"),
        ("--after", last.as_str()),
    ]);
    let empty = run_at("commons-inventory", &mut options, paths, &config).unwrap();
    assert_eq!(empty["page"]["records"], json!([]));
    assert!(empty["page"]["next_after"].is_null());

    let fork = crate::table::tests::seal(
        &session,
        scope,
        crate::crypto::Causal {
            sequence: 1,
            previous: None,
            parents: &[],
        },
        b"private paged inventory fork",
    )
    .unwrap();
    store.ingest(&session, &fork).unwrap();
    let before = std::fs::read(table_path.join("table.astream")).unwrap();
    let mut options = BTreeMap::from([("--table", table.as_str()), ("--records", "256")]);
    let quarantined = run_at("commons-inventory", &mut options, paths, &config).unwrap();
    assert_eq!(quarantined["known"]["count"], json!(6));
    assert_eq!(quarantined["eligible"]["count"], json!(0));
    let records = quarantined["page"]["records"].as_array().unwrap();
    assert_eq!(records.len(), 6);
    assert!(
        records
            .iter()
            .all(|record| record["eligible"] == json!(false))
    );
    assert!(!quarantined.to_string().contains("private paged inventory"));
    assert_eq!(
        std::fs::read(table_path.join("table.astream")).unwrap(),
        before
    );

    aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    let mut options = BTreeMap::from([("--table", table.as_str()), ("--records", "2")]);
    let excluded = run_at("commons-inventory", &mut options, paths, &config).unwrap();
    assert_eq!(excluded["storage"], json!("not-observed"));
    assert!(excluded["page"]["records"].is_null());
    assert!(excluded["page"]["next_after"].is_null());
    assert_eq!(
        std::fs::read(table_path.join("table.astream")).unwrap(),
        before
    );
}

#[test]
fn custody_inspection_respects_exclusions_without_needing_a_readable_catalog() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let descriptor = Descriptor::application(bootstrap.trust(), "custody-test", "rows", 1).unwrap();
    let scope = descriptor.scope;
    let config = paths.home.join("config");
    aterm_journal::create_private_dir_durable(directory.path(scope).unwrap().parent().unwrap())
        .unwrap();
    let mut writer = Store::join(&session, &directory.path(scope).unwrap(), scope).unwrap();
    writer
        .commit_catalog(&session, b"private-custody-payload")
        .unwrap();
    // No catalog exists. The independent claim can still bind the target.
    assert!(!directory.path(directory.catalog_scope()).unwrap().exists());
    let read =
        custody_inspection(paths, &config, scope.digest(), crate::admission::deadline()).unwrap();
    assert_eq!(read["claim"], json!("ready"));
    assert_eq!(read["receipt_prefix"]["state"], json!("verified"));
    assert_eq!(read["can_resume_writer"], json!(false));
    assert!(!read.to_string().contains("private-custody-payload"));
    aterm_uds::atomic_file::write(
        &config,
        format!(
            "[commons]\nexclude_tables = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(&scope.digest())
        )
        .as_bytes(),
        0o600,
    )
    .unwrap();
    // A damaged excluded target must remain uninspected.
    std::fs::write(
        directory.path(scope).unwrap().join("table.anchor"),
        b"damaged",
    )
    .unwrap();
    let excluded =
        custody_inspection(paths, &config, scope.digest(), crate::admission::deadline()).unwrap();
    assert_eq!(excluded["claim"], json!("not-inspected"));
    assert_eq!(excluded["reason"], json!("excluded"));
    assert_eq!(excluded["receipt_prefix"], Value::Null);
    std::fs::write(&config, "[commons]\nenabled = false\n").unwrap();
    let disabled =
        custody_inspection(paths, &config, scope.digest(), crate::admission::deadline()).unwrap();
    assert_eq!(disabled["reason"], json!("sharing-disabled"));
    assert_eq!(disabled["receipt_prefix"], Value::Null);
}

#[test]
fn retained_inventory_does_not_create_an_absent_catalog_or_undeclared_table() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(paths, bootstrap.trust());
    let config = paths.home.join("config");
    let catalog = inspect_inventory(paths, &config, directory.catalog_scope().digest()).unwrap();
    assert_eq!(catalog["subscription"], json!("catalog"));
    assert_eq!(catalog["storage"], json!("missing"));
    assert_eq!(catalog["known"], Value::Null);
    let unknown = inspect_inventory(paths, &config, [9; 32]).unwrap();
    assert_eq!(unknown["subscription"], json!("undeclared"));
    assert_eq!(unknown["storage"], json!("not-observed"));
    assert_eq!(unknown["known"], Value::Null);
    assert!(!paths.dir.join("commons/tables").exists());
    assert!(!paths.dir.join("commons/table-claims").exists());
}

#[test]
fn retained_inventory_distinguishes_missing_empty_and_committed_beside_writers() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let descriptor =
        Descriptor::application(bootstrap.trust(), "inventory-test", "rows", 1).unwrap();
    let scope = descriptor.scope;
    // A retained declaration can precede its application's storage.
    let mut declaration_log = directory.catalog(&session).unwrap();
    declaration_log
        .commit_catalog(&session, &descriptor.encode().unwrap())
        .unwrap();
    drop(declaration_log);
    let table_path = directory.path(scope).unwrap();
    let missing = inspect_inventory(paths, &config, scope.digest()).unwrap();
    assert_eq!(missing["subscription"], json!("subscribed"));
    assert_eq!(missing["storage"], json!("missing"));
    assert_eq!(missing["known"], Value::Null);
    assert!(!table_path.exists());

    // Native discovery reserves a private directory before enrollment.
    // Its lack of a claim or journal is not damage or an empty inventory.
    let subscribed = directory
        .subscriptions(&session, &Policy::default())
        .unwrap();
    assert!(subscribed.contains_key(&scope.digest()));
    assert_eq!(std::fs::read_dir(&table_path).unwrap().count(), 0);
    let claim = bootstrap
        .journal_dir()
        .parent()
        .unwrap()
        .join("table-claims")
        .join(format!(
            "{}.claim",
            aterm_codec::hex::encode_lower(&scope.digest())
        ));
    assert!(!claim.exists());
    let reserved = inspect_inventory(paths, &config, scope.digest()).unwrap();
    assert_eq!(reserved["storage"], json!("missing"));
    assert_eq!(reserved["known"], Value::Null);
    assert_eq!(reserved["eligible"], Value::Null);
    assert_eq!(std::fs::read_dir(&table_path).unwrap().count(), 0);
    assert!(!claim.exists());

    // Unclaimed nonempty storage is never treated as a reservation.
    let unknown = table_path.join("retained-unclaimed-record");
    std::fs::write(&unknown, b"retain this evidence").unwrap();
    assert!(inspect_inventory(paths, &config, scope.digest()).is_err());
    assert_eq!(std::fs::read(&unknown).unwrap(), b"retain this evidence");
    std::fs::rename(&unknown, paths.home.join("unclaimed-evidence")).unwrap();

    let mut store = Store::join(&session, &table_path, scope).unwrap();
    let empty = inspect_inventory(paths, &config, scope.digest()).unwrap();
    assert_eq!(empty["storage"], json!("present"));
    assert_eq!(empty["known"]["count"], json!(0));
    assert_eq!(empty["known"], empty["eligible"]);
    let written = store
        .commit_catalog(&session, b"inventory-private-sentinel")
        .unwrap();
    let _catalog = directory.catalog(&session).unwrap();
    let catalog_path = directory.path(directory.catalog_scope()).unwrap();
    let before_table = std::fs::read(table_path.join("table.astream")).unwrap();
    let before_catalog = std::fs::read(catalog_path.join("table.astream")).unwrap();
    let read = inspect_inventory(paths, &config, scope.digest()).unwrap();
    let expected = crate::replica::InventorySummary::of(&BTreeSet::from([written.id]));
    assert_eq!(read["known"]["count"], json!(1));
    assert_eq!(read["known"], read["eligible"]);
    assert_eq!(
        read["known"]["sha256"],
        json!(aterm_codec::hex::encode_lower(&expected.fingerprint))
    );
    assert!(!read.to_string().contains("inventory-private-sentinel"));
    let catalog = inspect_inventory(paths, &config, directory.catalog_scope().digest()).unwrap();
    assert_eq!(catalog["storage"], json!("present"));
    assert_eq!(catalog["known"]["count"], json!(1));
    assert_eq!(
        std::fs::read(table_path.join("table.astream")).unwrap(),
        before_table
    );
    assert_eq!(
        std::fs::read(catalog_path.join("table.astream")).unwrap(),
        before_catalog
    );
    // Losing a real claim must still refuse the retained journal.
    std::fs::rename(&claim, paths.home.join("retained-table-claim")).unwrap();
    assert!(inspect_inventory(paths, &config, scope.digest()).is_err());
    assert_eq!(
        std::fs::read(table_path.join("table.astream")).unwrap(),
        before_table
    );
}

#[test]
fn retained_inventory_does_not_inspect_conflicting_declarations() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let first = Descriptor::application(bootstrap.trust(), "inventory-test", "rows", 1).unwrap();
    let second = Descriptor::application(bootstrap.trust(), "inventory-test", "rows", 2).unwrap();
    assert_eq!(first.scope, second.scope);
    let mut catalog = directory.catalog(&session).unwrap();
    for descriptor in [first.clone(), second] {
        catalog
            .commit_catalog(&session, &descriptor.encode().unwrap())
            .unwrap();
    }
    let read = inspect_inventory(paths, &paths.home.join("config"), first.scope.digest()).unwrap();
    assert_eq!(read["subscription"], json!("conflict"));
    assert_eq!(read["storage"], json!("not-observed"));
    assert_eq!(read["known"], Value::Null);
    assert_eq!(read["eligible"], Value::Null);
    assert!(!directory.path(first.scope).unwrap().exists());
}

#[test]
fn retained_inventory_keeps_exclusions_and_rejects_damaged_established_storage() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let descriptor =
        Descriptor::application(bootstrap.trust(), "inventory-test", "rows", 1).unwrap();
    let scope = directory.publish(&session, &descriptor).unwrap();
    let table_path = directory.path(scope).unwrap();
    let mut store = Store::join(&session, &table_path, scope).unwrap();
    store
        .commit_catalog(&session, b"committed fixture record")
        .unwrap();
    drop(store);
    drop(session);
    drop(bootstrap);
    let log = table_path.join("table.astream");
    std::fs::write(&log, b"broken fixture journal").unwrap();
    let encoded = aterm_codec::hex::encode_lower(&scope.digest());
    aterm_uds::atomic_file::write(
        &config,
        format!("[commons]\nexclude_tables=[\"{encoded}\"]\n").as_bytes(),
        0o600,
    )
    .unwrap();
    let excluded = inspect_inventory(paths, &config, scope.digest()).unwrap();
    assert_eq!(excluded["subscription"], json!("excluded"));
    assert_eq!(excluded["storage"], json!("not-observed"));
    assert_eq!(excluded["known"], Value::Null);
    let catalog = directory.catalog_scope().digest();
    let encoded_catalog = aterm_codec::hex::encode_lower(&catalog);
    std::fs::write(
        &config,
        format!("[commons]\nexclude_tables=[\"{encoded_catalog}\"]\n"),
    )
    .unwrap();
    assert_eq!(
        inspect_inventory(paths, &config, catalog).unwrap()["known"],
        Value::Null
    );
    std::fs::write(&config, "[commons]\nenabled=false\n").unwrap();
    let disabled = inspect_inventory(paths, &config, catalog).unwrap();
    assert_eq!(disabled["subscription"], json!("excluded"));
    assert_eq!(disabled["storage"], json!("not-observed"));
    std::fs::write(&config, "[commons]\n").unwrap();
    assert!(inspect_inventory(paths, &config, scope.digest()).is_err());
    assert_eq!(std::fs::read(log).unwrap(), b"broken fixture journal");
}

#[test]
fn legacy_readiness_distinguishes_the_vault_from_commons_without_creating_a_network() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&home.path().join("m1"));
    assert_eq!(status(&paths)["state"].as_str(), Some("vault-unconfigured"));
    assert!(!paths.home.exists());
    aterm_secrets::client::test_create_unbound(
        &paths,
        home.path().join("test.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let ready = status(&paths);
    assert_eq!(ready["state"].as_str(), Some("control-not-visible"));
    assert_eq!(ready["can_initialize_network"].as_bool(), Some(false));
    assert_eq!(ready["ready"].as_bool(), Some(false));
    assert!(!paths.dir.join("commons").exists());
    let error = run_at(
        "commons-tables",
        &mut BTreeMap::new(),
        &paths,
        &paths.home.join("config"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("control-not-visible"));
}

#[test]
fn a_new_vault_has_a_signed_founder_and_can_open_its_catalog() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&home.path().join("m1"));
    aterm_secrets::client::create(
        &paths,
        home.path().join("test.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let ready = status(&paths);
    assert_eq!(ready["state"].as_str(), Some("ready"));
    assert_eq!(ready["ready"].as_bool(), Some(true));
    assert_eq!(ready["role"].as_str(), Some("writer"));
    assert_eq!(ready["sharing_enabled"].as_bool(), Some(true));
    assert!(!paths.dir.join("commons/tables").exists());
    let listing = run_at(
        "commons-tables",
        &mut BTreeMap::new(),
        &paths,
        &paths.home.join("config"),
    )
    .unwrap();
    assert!(listing["tables"].as_array().unwrap().is_empty());
    assert!(!paths.dir.join("commons/tables").exists());
    let tables = paths.dir.join("commons/tables");
    std::os::unix::fs::symlink(home.path().join("absent"), &tables).unwrap();
    assert!(
        run_at(
            "commons-tables",
            &mut BTreeMap::new(),
            &paths,
            &paths.home.join("config")
        )
        .is_err()
    );
    assert!(
        std::fs::symlink_metadata(&tables)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn readiness_reports_roles_pending_enrollment_and_removed_membership() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Reader]);
    for (index, role) in [(0, "writer"), (1, "reader")] {
        lab.bootstrap(index);
        let ready = status(&lab.paths[index]);
        assert_eq!(ready["ready"].as_bool(), Some(true));
        assert_eq!(ready["role"].as_str(), Some(role));
        assert!(!lab.paths[index].dir.join("commons/tables").exists());
    }
    aterm_uds::atomic_file::write(
        &lab.paths[0].home.join("config"),
        b"[commons]\nenabled=false\n",
        0o600,
    )
    .unwrap();
    assert_eq!(
        status(&lab.paths[0])["sharing_enabled"].as_bool(),
        Some(false)
    );
    lab.rotate(&[Role::Writer]);
    assert_eq!(
        status(&lab.paths[1])["state"].as_str(),
        Some("membership-inactive")
    );
    let pending = crate::table::tests::Lab::with_enrolled(&[Role::Writer, Role::Writer], 1);
    pending.bootstrap(1);
    assert_eq!(
        status(&pending.paths[1])["state"].as_str(),
        Some("enrollment-pending")
    );
}

#[test]
fn readiness_reports_an_unbound_log_recipient_as_pending_without_granting_access() {
    use aterm_secrets::{
        client::{load_records, save_records, seed_vault_genesis},
        commons::roster::Role,
    };
    let lab = crate::table::tests::Lab::with_enrolled(&[Role::Writer, Role::Writer], 1);
    seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
    let seeded = load_records(&lab.paths[0]).unwrap().encode();
    save_records(&lab.paths[1], &seeded).unwrap();
    let bootstrap = lab.bootstrap(1);
    let request = bootstrap.request().unwrap();
    let store = load_records(&lab.paths[1]).unwrap();
    let log = store.log().unwrap();
    assert!(!log.has_known_actor(&request.binding().actor()));
    assert!(!log.has_historical_seat(request.binding()));
    assert!(bootstrap.log_witness().unwrap().is_none());
    assert!(Session::observe(&bootstrap).is_err());

    let observed = status(&lab.paths[1]);
    assert_eq!(observed["state"], json!("enrollment-pending"), "{observed}");
    assert_eq!(observed["ready"], json!(false));
    assert_eq!(observed["role"], Value::Null);
    assert_eq!(load_records(&lab.paths[1]).unwrap().encode(), seeded);
    assert!(!lab.paths[1].dir.join("commons/tables").exists());
}

#[test]
fn commons_status_reads_control_history_while_a_writer_holds_it() {
    use aterm_secrets::commons::{journal::Journal, roster::Role};
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let bootstrap = lab.bootstrap(0);
    for expected_nodes in 1..=2 {
        if expected_nodes == 2 {
            lab.rotate(&[Role::Writer]);
        }
        let held = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
        let log = bootstrap.journal_dir().join("control.astream");
        let before = std::fs::read(&log).unwrap();
        // Observe the acknowledged prefix beside a writer, including its later rotation.
        for _ in 0..2 {
            let read = status(paths);
            assert_eq!(read["state"].as_str(), Some("ready"), "{read}");
            assert_eq!(read["ready"].as_bool(), Some(true));
            assert_eq!(read["can_initialize_network"].as_bool(), Some(false));
            let inventory = &read["control_inventory"];
            assert_eq!(inventory["retained_nodes"], json!(expected_nodes));
            assert_eq!(inventory["largest_fact_count"].as_u64(), Some(2));
            assert_eq!(
                inventory["fact_limit"],
                json!(aterm_secrets::commons::constraints::MAX_FACTS)
            );
            assert_eq!(
                inventory["legacy_fact_limit"],
                json!(aterm_secrets::commons::constraints::LEGACY_MAX_FACTS)
            );
        }
        assert_eq!(std::fs::read(log).unwrap(), before);
        assert_eq!(held.inventory().count(), expected_nodes);
    }
    assert!(!paths.dir.join("commons/tables").exists());
    assert_eq!(status(paths)["state"].as_str(), Some("ready"));
}

#[test]
fn table_listing_reports_recovery_and_journal_rings_without_writing() {
    use aterm_secrets::commons::roster::Role;

    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    let bootstrap = lab.bootstrap(0);
    let now = aterm_time::unix_millis().unwrap() / 1000;
    let retained: Vec<_> = {
        let session = Session::open(&bootstrap).unwrap();
        let directory = Directory::new(paths, bootstrap.trust());
        [&ring::RECOVERIES, &ring::JOURNAL]
            .into_iter()
            .map(|ring| {
                let epoch = ring.epoch_of(now);
                let scope = ring.scope(directory.trust(), epoch).unwrap();
                directory
                    .enroll_ring(&session, ring, epoch, now)
                    .unwrap()
                    .commit_own(&session, b"private ring inventory fixture")
                    .unwrap();
                (ring, epoch, aterm_codec::hex::encode_lower(&scope.digest()))
            })
            .collect()
    };
    let before = crate::tests::files(lab._home.path());
    let listing = run_at("commons-tables", &mut BTreeMap::new(), paths, &config).unwrap();
    let rings = listing["rings"].as_array().unwrap();
    for (ring, epoch, table) in retained {
        let entry = rings
            .iter()
            .find(|entry| entry["table"].as_str() == Some(table.as_str()))
            .unwrap_or_else(|| panic!("{} / {} ring is absent", ring.application, ring.role));
        assert_eq!(entry["application"], json!(ring.application));
        assert_eq!(entry["role"], json!(ring.role));
        assert_eq!(entry["period_s"], json!(ring.period_s));
        assert_eq!(entry["epoch"], json!(epoch));
        assert_eq!(entry["retained"], json!(1));
    }
    assert!(rings.iter().any(|entry| entry["retained"].is_null()));
    assert!(
        !listing
            .to_string()
            .contains("private ring inventory fixture")
    );
    assert!(crate::tests::files(lab._home.path()) == before);
}

#[test]
fn table_listing_tolerates_damaged_ring_state() {
    check_ring_listing_with_bad_state(false);
}

#[test]
fn table_listing_tolerates_unreadable_ring_state() {
    check_ring_listing_with_bad_state(true);
}

fn check_ring_listing_with_bad_state(state_is_directory: bool) {
    use aterm_secrets::commons::roster::Role;

    for ring in ring::RINGS {
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let config = paths.home.join("config");
        let bootstrap = lab.bootstrap(0);
        let now = aterm_time::unix_millis().unwrap() / 1000;
        let (table, epoch, state) = {
            let session = Session::open(&bootstrap).unwrap();
            let directory = Directory::new(paths, bootstrap.trust());
            let epoch = ring.epoch_of(now);
            let scope = ring.scope(directory.trust(), epoch).unwrap();
            directory
                .enroll_ring(&session, ring, epoch, now)
                .unwrap()
                .commit_own(&session, b"private damaged-ring fixture")
                .unwrap();
            let claims = crate::table::store::claim_dir(&session).unwrap();
            let state = claims.join(ring.state_name());
            if state_is_directory {
                std::fs::rename(&state, lab._home.path().join("original-ring-state")).unwrap();
                std::fs::create_dir(&state).unwrap();
            } else {
                aterm_journal::write_private_durable(
                    &claims,
                    &ring.state_name(),
                    b"not a ring state",
                )
                .unwrap();
            }
            (
                aterm_codec::hex::encode_lower(&scope.digest()),
                epoch,
                state,
            )
        };
        let before = crate::tests::files(lab._home.path());
        let inventory = run_at(
            "commons-inventory",
            &mut BTreeMap::from([("--table", table.as_str())]),
            paths,
            &config,
        )
        .unwrap();
        assert_eq!(inventory["known"]["count"], json!(1));
        let listing = run_at("commons-tables", &mut BTreeMap::new(), paths, &config)
            .expect("a read-only listing must tolerate invalid local ring metadata");
        let entry = listing["rings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["table"].as_str() == Some(table.as_str()))
            .expect("the retained table must remain in the reader's window");
        assert_eq!(entry["epoch"], json!(epoch));
        assert_eq!(entry["retained"], json!(1));
        assert!(!listing.to_string().contains("private damaged-ring fixture"));
        assert_eq!(state.is_dir(), state_is_directory);
        assert!(crate::tests::files(lab._home.path()) == before);
    }
}

#[test]
fn table_listing_reports_ring_subscription_policy() {
    use aterm_secrets::commons::roster::Role;

    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    let (table, _) = retained_ring_fixture(&lab, &ring::JOURNAL);
    for (policy, expected, other_state) in [
        (String::new(), "subscribed", "subscribed"),
        ("[commons]\nenabled=false\n".into(), "excluded", "excluded"),
        ("[swarm]\nenabled=false\n".into(), "excluded", "excluded"),
        (
            format!("[commons]\nexclude_tables=[\"{table}\"]\n"),
            "excluded",
            "subscribed",
        ),
    ] {
        aterm_secrets::fsx::write_atomic(&config, policy.as_bytes(), 0o600).unwrap();
        let before = crate::tests::files(lab._home.path());
        let listing = run_at("commons-tables", &mut BTreeMap::new(), paths, &config).unwrap();
        let entry = listing["rings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["table"].as_str() == Some(table.as_str()))
            .unwrap();
        assert_eq!(entry["state"], json!(expected));
        for other in listing["rings"].as_array().unwrap() {
            if other["table"].as_str() != Some(table.as_str()) {
                assert_eq!(other["state"], json!(other_state));
            }
        }
        assert_eq!(
            entry["retained"],
            if expected == "subscribed" {
                json!(1)
            } else {
                Value::Null
            }
        );
        assert!(crate::tests::files(lab._home.path()) == before);
    }
}

#[test]
fn table_listing_does_not_inspect_excluded_ring_storage() {
    use aterm_secrets::commons::roster::Role;

    for ring in ring::RINGS {
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let config = paths.home.join("config");
        let (table, log) = retained_ring_fixture(&lab, ring);
        std::fs::write(&log, b"damaged excluded fixture").unwrap();
        let catalog = {
            let bootstrap = lab.bootstrap(0);
            aterm_codec::hex::encode_lower(
                &Directory::new(paths, bootstrap.trust())
                    .catalog_scope()
                    .digest(),
            )
        };
        for policy in [
            "[commons]\nenabled=false\n".into(),
            "[swarm]\nenabled=false\n".into(),
            format!("[commons]\nexclude_tables=[\"{table}\"]\n"),
            format!("[commons]\nexclude_tables=[\"{catalog}\"]\n"),
        ] {
            aterm_secrets::fsx::write_atomic(&config, policy.as_bytes(), 0o600).unwrap();
            let before = crate::tests::files(lab._home.path());
            let listing = run_at("commons-tables", &mut BTreeMap::new(), paths, &config)
                .expect("an excluded ring's storage must remain unobserved");
            let entry = listing["rings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["table"].as_str() == Some(table.as_str()))
                .unwrap();
            assert_eq!(entry["state"], json!("excluded"));
            assert!(entry["retained"].is_null());
            let inventory = run_at(
                "commons-inventory",
                &mut BTreeMap::from([("--table", table.as_str())]),
                paths,
                &config,
            )
            .unwrap();
            assert_eq!(inventory["subscription"], json!("excluded"));
            assert_eq!(inventory["storage"], json!("not-observed"));
            assert!(inventory["known"].is_null());
            let custody = run_at(
                "commons-custody",
                &mut BTreeMap::from([("--table", table.as_str())]),
                paths,
                &config,
            )
            .unwrap();
            assert_eq!(custody["claim"], json!("not-inspected"));
            assert!(custody["directory"].is_null());
            assert!(crate::tests::files(lab._home.path()) == before);
        }
        aterm_secrets::fsx::write_atomic(&config, b"", 0o600).unwrap();
        assert!(run_at("commons-tables", &mut BTreeMap::new(), paths, &config).is_err());
    }
}

fn retained_ring_fixture(
    lab: &crate::table::tests::Lab,
    ring: &ring::Ring,
) -> (String, std::path::PathBuf) {
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let now = aterm_time::unix_millis().unwrap() / 1000;
    let epoch = ring.epoch_of(now);
    let scope = ring.scope(directory.trust(), epoch).unwrap();
    directory
        .enroll_ring(&session, ring, epoch, now)
        .unwrap()
        .commit_own(&session, b"private ring policy fixture")
        .unwrap();
    (
        aterm_codec::hex::encode_lower(&scope.digest()),
        directory.path(scope).unwrap().join("table.astream"),
    )
}

#[test]
fn table_listing_includes_recovery_only_ring_storage() {
    check_recovery_ring_listing(false);
}

#[test]
fn table_listing_combines_primary_and_recovery_ring_storage() {
    check_recovery_ring_listing(true);
}

fn check_recovery_ring_listing(keep_primary: bool) {
    use aterm_secrets::commons::roster::Role;

    for ring in ring::RINGS {
        let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
        let paths = &lab.paths[0];
        let config = paths.home.join("config");
        let now = aterm_time::unix_millis().unwrap() / 1000;
        let (scope, primary_wire) = recovery_listing_wire(&lab, 0, ring, now);
        let table = aterm_codec::hex::encode_lower(&scope.digest());
        let primary_listing =
            run_at("commons-tables", &mut BTreeMap::new(), paths, &config).unwrap();
        let recovered_wire = if keep_primary {
            let (other_scope, wire) = recovery_listing_wire(&lab, 1, ring, now);
            assert_eq!(other_scope, scope);
            wire
        } else {
            primary_wire
        };
        let recovery_log = {
            let bootstrap = lab.bootstrap(0);
            let session = Session::open(&bootstrap).unwrap();
            let directory = Directory::new(paths, bootstrap.trust());
            if !keep_primary {
                std::fs::rename(
                    directory.path(scope).unwrap(),
                    lab._home.path().join("original-ring"),
                )
                .unwrap();
            }
            let recovery = directory.recovery();
            recovery
                .enroll(&session, scope)
                .unwrap()
                .ingest(&session, &recovered_wire)
                .unwrap();
            recovery.path(scope).unwrap().join("table.astream")
        };
        let before = crate::tests::files(lab._home.path());
        let expected = if keep_primary { 2 } else { 1 };
        let detailed = inspect_inventory(paths, &config, scope.digest()).unwrap();
        assert_eq!(detailed["known"]["count"], json!(expected));
        assert_eq!(detailed["recovery_reader"], json!(true));
        assert_eq!(detailed["writer_resume_authorized"], json!(false));
        let listing = run_at("commons-tables", &mut BTreeMap::new(), paths, &config).unwrap();
        let entry = listing["rings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["table"].as_str() == Some(table.as_str()))
            .unwrap();
        assert_eq!(entry["retained"], detailed["known"]["count"]);
        assert_eq!(entry["recovery_reader"], json!(true));
        let primary_entry = primary_listing["rings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["table"].as_str() == Some(table.as_str()))
            .unwrap();
        assert_eq!(primary_entry["retained"], json!(1));
        assert_eq!(primary_entry["recovery_reader"], json!(false));
        assert!(
            !listing
                .to_string()
                .contains("private recovery inventory fixture")
        );
        assert!(crate::tests::files(lab._home.path()) == before);

        aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
        std::fs::write(&recovery_log, b"damaged excluded recovery fixture").unwrap();
        let before = crate::tests::files(lab._home.path());
        let excluded = run_at("commons-tables", &mut BTreeMap::new(), paths, &config).unwrap();
        let entry = excluded["rings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["table"].as_str() == Some(table.as_str()))
            .unwrap();
        assert_eq!(entry["state"], json!("excluded"));
        assert!(entry["retained"].is_null());
        assert!(entry["recovery_reader"].is_null());
        assert!(crate::tests::files(lab._home.path()) == before);
        aterm_secrets::fsx::write_atomic(&config, b"", 0o600).unwrap();
        let before = crate::tests::files(lab._home.path());
        assert!(run_at("commons-tables", &mut BTreeMap::new(), paths, &config).is_err());
        assert!(crate::tests::files(lab._home.path()) == before);
    }
}

fn recovery_listing_wire(
    lab: &crate::table::tests::Lab,
    actor: usize,
    ring: &ring::Ring,
    now: u64,
) -> (Scope, Vec<u8>) {
    let bootstrap = lab.bootstrap(actor);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[actor], bootstrap.trust());
    let epoch = ring.epoch_of(now);
    let scope = ring.scope(directory.trust(), epoch).unwrap();
    let mut store = directory.enroll_ring(&session, ring, epoch, now).unwrap();
    let receipt = store
        .commit_own(&session, b"private recovery inventory fixture")
        .unwrap();
    let wire = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
    (scope, wire)
}

#[test]
fn table_inventory_reads_beside_held_control_and_catalog_writers() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    run_at(
        "commons-open",
        &mut BTreeMap::from([("--application", "inventory-test"), ("--role", "rows")]),
        paths,
        &config,
    )
    .unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let held = directory.catalog(&session).unwrap();
    let catalog_path = directory.path(directory.catalog_scope()).unwrap();
    let before = std::fs::read(catalog_path.join("table.astream")).unwrap();
    let listing = run_at("commons-tables", &mut BTreeMap::new(), paths, &config).unwrap();
    assert_eq!(listing["tables"].as_array().unwrap().len(), 1);
    assert_eq!(
        listing["tables"][0]["declarations"][0]["application"].as_str(),
        Some("inventory-test")
    );
    assert_eq!(
        std::fs::read(catalog_path.join("table.astream")).unwrap(),
        before
    );
    assert!(held.inventory(&session).unwrap().next().is_some());
}

/// A read whose wait has already run out tries each lock once and waits on none, so the
/// custody held past its wait is met at once. The held custody, not the spent deadline, makes
/// it busy: the same read is ready once custody is let go.
#[test]
fn commons_status_beside_a_custody_observation_held_past_its_wait_is_busy() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    lab.bootstrap(0);
    let paths = &lab.paths[0];
    let config = paths.home.join("config");
    let past_its_wait = || commons_status_until(paths, &config, std::time::Instant::now()).unwrap();
    let custody = aterm_uds::private_file::PrivateLock::take(
        &paths.dir,
        "commons.lock",
        std::time::Duration::ZERO,
        "held",
    )
    .unwrap();
    let busy = past_its_wait();
    assert_eq!(busy["state"].as_str(), Some("control-busy"), "{busy}");
    assert_eq!(busy["membership_checked"].as_bool(), Some(false));
    assert_eq!(busy["ready"].as_bool(), Some(false));
    assert!(busy.get("role").is_none());
    assert!(busy.get("network_id").is_none());
    drop(custody);
    assert_eq!(past_its_wait()["state"].as_str(), Some("ready"));
    assert_eq!(status(paths)["state"].as_str(), Some("ready"));
}

#[test]
fn readiness_refuses_lost_control_history_and_bad_options_without_reset() {
    use aterm_secrets::commons::roster::Role;
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let log = bootstrap.journal_dir().join("control.astream");
    std::fs::remove_file(&log).unwrap();
    assert!(
        run_at(
            "commons-status",
            &mut BTreeMap::new(),
            &lab.paths[0],
            &lab.paths[0].home.join("config")
        )
        .is_err()
    );
    assert!(!log.exists());
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&home.path().join("unused"));
    assert!(
        run_at(
            "commons-status",
            &mut BTreeMap::from([("--unknown", "value")]),
            &paths,
            &paths.home.join("config")
        )
        .is_err()
    );
    assert!(!paths.home.exists());
}

#[test]
fn commands_publish_reopen_and_report_signed_provenance() {
    let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    let config = lab.paths[0].home.join("config");
    let options = || BTreeMap::from([("--application", "accounts"), ("--role", "proposals")]);
    let first = run_at("commons-open", &mut options(), &lab.paths[0], &config).unwrap();
    let second = run_at("commons-open", &mut options(), &lab.paths[0], &config).unwrap();
    assert_eq!(first, second);
    let listing = run_at(
        "commons-tables",
        &mut BTreeMap::new(),
        &lab.paths[0],
        &config,
    )
    .unwrap();
    let declarations = listing["tables"][0]["declarations"].as_array().unwrap();
    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0]["application"].as_str(), Some("accounts"));
    assert!(declarations[0]["actor"].as_str().is_some());
    assert!(declarations[0]["authored_ms"].as_u64().is_some());
}

#[test]
fn commons_status_reports_membership_inactive_when_superseded_by_another_recipient_seat_in_log() {
    use aterm_secrets::{
        client::{admit_member_in_log, configure_log_member, load_records, save_records},
        commons::{bootstrap::Device, roster::Role},
        membership_log::{Entrant, Evidence},
    };
    let lab = crate::table::tests::Lab::with_foundation(&[Role::Writer, Role::Writer]);
    aterm_secrets::client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2"))
        .unwrap();
    let seeded = load_records(&lab.paths[0]).unwrap().encode();
    save_records(&lab.paths[1], &seeded).unwrap();

    let trust = lab.bootstrap(0).trust();
    let pinned = format!(
        "{}#{}",
        lab._home.path().join("test.vault").display(),
        lab.signer
    );
    let m3_a = Paths::for_home(&lab._home.path().join("m3-a"));
    let m3_b = Paths::for_home(&lab._home.path().join("m3-b"));
    configure_log_member(&m3_a, &pinned, "m3").unwrap();
    configure_log_member(&m3_b, &pinned, "m3").unwrap();
    let _ = aterm_secrets::client::machine_identity(&m3_a).unwrap();
    let identity_bytes = std::fs::read(m3_a.dir.join("identity")).unwrap();
    aterm_secrets::fsx::write_atomic(&m3_b.dir.join("identity"), &identity_bytes, 0o600).unwrap();
    let req_a = Device::open(&m3_a)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let req_b = Device::open(&m3_b)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    assert_ne!(req_a.binding(), req_b.binding());

    // Concurrently admit `req_a` on `m1` and `req_b` on `m2` (both for `m3`'s recipient), then
    // merge the two branches so both seats are active in `log.membership().members()`.
    admit_member_in_log(
        &lab.paths[0],
        Entrant {
            request: req_a,
            node_key: [0x61; 32],
            name: "m3".to_owned(),
            label: "m3".to_owned(),
        },
        Evidence::Phrase,
    )
    .unwrap();
    admit_member_in_log(
        &lab.paths[1],
        Entrant {
            request: req_b,
            node_key: [0x62; 32],
            name: "m3".to_owned(),
            label: "m3".to_owned(),
        },
        Evidence::Phrase,
    )
    .unwrap();
    save_records(
        &lab.paths[0],
        &load_records(&lab.paths[1]).unwrap().encode(),
    )
    .unwrap();
    let merged = load_records(&lab.paths[0]).unwrap().encode();
    save_records(&m3_a, &merged).unwrap();
    save_records(&m3_b, &merged).unwrap();

    Bootstrap::required(&m3_a).unwrap();
    Bootstrap::required(&m3_b).unwrap();
    let status_a = status(&m3_a);
    let status_b = status(&m3_b);
    let states = [
        status_a["state"].as_str().unwrap(),
        status_b["state"].as_str().unwrap(),
    ];
    assert!(
        states == ["ready", "membership-inactive"] || states == ["membership-inactive", "ready"],
        "expected one ready and one membership-inactive seat, got: {status_a} / {status_b}"
    );
}
