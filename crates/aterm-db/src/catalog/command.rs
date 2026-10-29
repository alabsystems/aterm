// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::{Value, json};
use aterm_secrets::commons::bootstrap::Bootstrap;

pub(crate) fn run(command: &str, mut options: BTreeMap<&str, &str>) -> Result<Value> {
    let paths = Paths::live().map_err(crate::table::failure)?;
    let config = aterm_types::dirs::aterm_config_path()
        .ok_or_else(|| Error::new("Commons configuration path is unavailable"))?;
    run_at(command, &mut options, &paths, &config)
}
fn run_at(
    command: &str,
    options: &mut BTreeMap<&str, &str>,
    paths: &Paths,
    config: &Path,
) -> Result<Value> {
    // Parse all arguments before consulting or creating local custody.
    if command == "commons-inventory" {
        let encoded = options
            .remove("--table")
            .ok_or_else(|| Error::new("missing --table"))?;
        let table: Digest = if encoded.len() == 64 {
            aterm_codec::hex::decode_vec(encoded.as_bytes()).and_then(|bytes| bytes.try_into().ok())
        } else {
            None
        }
        .ok_or_else(|| Error::new("invalid --table digest"))?;
        if !options.is_empty() {
            return Err(Error::new("unknown Commons inventory option"));
        }
        return retained_inventory(paths, config, table);
    }
    let descriptor = if matches!(
        command,
        "commons-tables" | "commons-status" | "commons-authority"
    ) {
        None
    } else {
        let application = options
            .remove("--application")
            .ok_or_else(|| Error::new("missing --application"))?;
        let role = options
            .remove("--role")
            .ok_or_else(|| Error::new("missing --role"))?;
        let version = options
            .remove("--version")
            .unwrap_or("1")
            .parse::<u32>()
            .map_err(|_| Error::new("invalid --version"))?;
        let name = options.remove("--name").map(str::to_owned);
        if command == "commons-open" && name.is_some() {
            return Err(Error::new(
                "application table names are fixed by their role",
            ));
        }
        Some((application, role, version, name))
    };
    if !options.is_empty() {
        return Err(Error::new("unknown Commons table option"));
    }
    if command == "commons-status" {
        return readiness(paths, config);
    }
    if command == "commons-authority" {
        return authority(paths);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    if let Some((application, role, version, name)) = descriptor {
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal(&bootstrap, journal)?;
        let directory = Directory::new(paths, bootstrap.trust());
        let mut descriptor = if command == "commons-open" {
            Descriptor::application(bootstrap.trust(), application, role, version)?
        } else {
            Descriptor::fresh(bootstrap.trust(), application, role, version)?
        };
        descriptor.name = name;
        let scope = directory.publish(&session, &descriptor)?;
        return Ok(
            json!({"table":aterm_codec::hex::encode_lower(&scope.digest()),
            "uuid":aterm_codec::hex::encode_lower(scope.table()), "generation":scope.generation(),
            "application":application, "role":role, "version":version}),
        );
    }
    let bootstrap = Bootstrap::required_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::observe(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let snapshot = directory.observe_with_config(&session, config)?;
    let rings = ring_tables(&session, &directory)?;
    let entries: Vec<_> = snapshot.entries.iter().map(|(id,entry)| {
        json!({"table":aterm_codec::hex::encode_lower(id),
            "uuid":aterm_codec::hex::encode_lower(entry.scope.table()),
            "generation":entry.scope.generation(), "state":match entry.state {
            State::Subscribed=>"subscribed", State::Conflict=>"conflict", State::Excluded=>"excluded",
            State::CreatorLimit=>"creator-limit", State::Capacity=>"capacity"},
            "declarations":entry.declarations.iter().map(|d| json!({
                "application":d.descriptor.application, "role":d.descriptor.role,
                "version":d.descriptor.version, "name":d.descriptor.name,
                "operation":aterm_codec::hex::encode_lower(&d.operation),
                "actor":aterm_codec::hex::encode_lower(&d.actor), "authored_ms":d.authored_ms
            })).collect::<Vec<_>>()})
    }).collect();
    Ok(
        json!({"network":"ALab Commons", "network_id":aterm_codec::hex::encode_lower(&bootstrap.trust().network),
        "catalog":aterm_codec::hex::encode_lower(&directory.catalog_scope().digest()),
        "tables":entries, "rings":rings, "inert":snapshot.inert}),
    )
}

/// Each ring's window tables, by period: their identity and how many operations this Mac retains
/// of each (none where it holds no storage for it).
fn ring_tables(session: &Session<'_>, directory: &Directory) -> Result<Vec<Value>> {
    let now = aterm_time::unix_millis()
        .map(|ms| ms / 1000)
        .ok_or_else(|| Error::new("system clock cannot place the ring tables"))?;
    let ring = &ring::CLAIMS;
    let state = directory.ring_state(session, ring)?;
    let mut tables = Vec::new();
    for epoch in state.window(ring, now) {
        let scope = ring.scope(directory.trust(), epoch)?;
        let retained = Store::observe_if_exists(session, &directory.path(scope)?, scope)?
            .map(|store| store.inventory(session).map(Iterator::count))
            .transpose()?;
        tables.push(
            json!({"application":ring.application, "role":ring.role, "epoch":epoch,
            "table":aterm_codec::hex::encode_lower(&scope.digest()), "retained":retained}),
        );
    }
    Ok(tables)
}

fn retained_inventory(paths: &Paths, config: &Path, table: Digest) -> Result<Value> {
    use crate::replica::InventorySummary;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let bootstrap = Bootstrap::required_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::observe(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let catalog = directory.catalog_scope();
    let policy = Policy::read(config)?;
    let (subscription, scope) = if !policy.enabled || policy.excluded.contains(&table) {
        ("excluded", None)
    } else if table == catalog.digest() {
        ("catalog", Some(catalog))
    } else if let Some((_, _, scope)) = directory.ring_of(
        &session,
        table,
        aterm_time::unix_millis()
            .map(|ms| ms / 1000)
            .ok_or_else(|| Error::new("system clock cannot place the ring tables"))?,
    )? {
        ("ring", Some(scope))
    } else {
        let snapshot = directory.observe_with_config(&session, config)?;
        match snapshot.entries.get(&table) {
            None => ("undeclared", None),
            Some(entry) => match entry.state {
                State::Subscribed => ("subscribed", Some(entry.scope)),
                State::Conflict => ("conflict", None),
                State::Excluded => ("excluded", None),
                State::CreatorLimit => ("creator-limit", None),
                State::Capacity => ("capacity", None),
            },
        }
    };
    let summary = |inventory: InventorySummary| {
        json!({"count":inventory.count,
            "sha256":aterm_codec::hex::encode_lower(&inventory.fingerprint)})
    };
    let reply = |storage: &str, inventories: Option<(InventorySummary, InventorySummary)>| {
        json!({
            "kind":"commons-inventory", "scope":"local-retained-operation-ids",
            "network_id":aterm_codec::hex::encode_lower(&bootstrap.trust().network),
            "table":aterm_codec::hex::encode_lower(&table), "subscription":subscription,
            "storage":storage, "known":inventories.map(|(known,_)|summary(known)),
            "eligible":inventories.map(|(_,eligible)|summary(eligible)),
        })
    };
    let Some(scope) = scope else {
        return Ok(reply("not-observed", None));
    };
    let Some(store) = Store::observe_if_exists(&session, &directory.path(scope)?, scope)? else {
        return Ok(reply("missing", None));
    };
    let known: BTreeSet<_> = store.inventory(&session)?.copied().collect();
    let eligible: BTreeSet<_> = store.replication_inventory(&session)?.copied().collect();
    Ok(reply(
        "present",
        Some((
            InventorySummary::of(&known),
            InventorySummary::of(&eligible),
        )),
    ))
}

fn authority(paths: &Paths) -> Result<Value> {
    let admin = match aterm_secrets::client::admin_cached(paths) {
        Ok(admin) => admin,
        Err(_) => {
            return Ok(json!({
                "kind": "commons-authority", "scope": "locally-applied-vault",
                "authority_available": false,
                "detail": "cached administrator state could not be authenticated or opened",
                "can_initialize_network": false,
            }));
        }
    };
    let inspection = admin
        .inspect_commons_control()
        .map_err(crate::table::failure)?;
    let descriptor = inspection.descriptor.map(|trust| {
        json!({
            "network_id": aterm_codec::hex::encode_lower(&trust.network),
            "authority": aterm_codec::hex::encode_lower(&trust.authority),
            "genesis": aterm_codec::hex::encode_lower(&trust.genesis),
        })
    });
    Ok(json!({
        "kind": "commons-authority", "scope": "locally-applied-vault",
        "authority_available": true, "vault_version": inspection.vault_version,
        "authority": aterm_codec::hex::encode_lower(&inspection.authority),
        "lifetime_bound": inspection.lifetime_bound,
        "control": if inspection.descriptor.is_some() { "typed-descriptor" }
            else if inspection.entry_present { "opaque-entry" } else { "absent" },
        "descriptor": descriptor, "can_initialize_network": false,
    }))
}

fn readiness(paths: &Paths, config: &Path) -> Result<Value> {
    use aterm_secrets::commons::{
        bootstrap::Availability,
        constraints::{Fact, LEGACY_MAX_FACTS, MAX_FACTS},
        journal::State as ControlState,
        roster::Role,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let sharing = Policy::read(config)?.enabled;
    let Some(observation) = control_observation(Bootstrap::observe_until(paths, deadline))? else {
        return Ok(control_busy(sharing));
    };
    let bootstrap = match observation {
        Availability::Unavailable(reason) => {
            return Ok(json!({
                "state": reason.code(), "detail": reason.explanation(),
                    "ready": false, "sharing_enabled": sharing, "can_initialize_network": false,
            }));
        }
        Availability::Available(bootstrap) => bootstrap,
    };
    // The acknowledged prefix, read beside any writer: no lock to find busy.
    let journal = aterm_secrets::commons::journal::Journal::read_existing(
        bootstrap.journal_dir(),
        bootstrap.trust(),
    )
    .map_err(crate::table::failure)?;
    bootstrap
        .revalidate_custody()
        .map_err(crate::table::failure)?;
    let request = bootstrap.request().map_err(crate::table::failure)?;
    let mut role = None;
    let state = if let Some((log, _)) =
        aterm_secrets::client::trusted_commons_log(bootstrap.paths(), bootstrap.trust())
            .map_err(crate::table::failure)?
    {
        let actor = request.binding().actor();
        let active_member = log
            .membership()
            .members()
            .get(&actor)
            .is_some_and(|m| &m.binding == request.binding());
        if !active_member {
            if log.has_historical_seat(request.binding())
                || log.recipient_is_removed(&request.binding().recipient())
            {
                "membership-inactive"
            } else {
                "enrollment-pending"
            }
        } else if let Some(access) = bootstrap
            .access_in(&journal)
            .map_err(crate::table::failure)?
        {
            role = Some(match access.local().role() {
                Role::Reader => "reader",
                Role::Writer => "writer",
            });
            "ready"
        } else {
            "history-pending"
        }
    } else {
        match journal.state() {
            ControlState::Ready { basis, .. } => {
                let binds_local = |node: &aterm_secrets::commons::node::Node| {
                    node.facts().iter().any(
                        |fact| matches!(fact, Fact::Bind { binding } if binding == request.binding()),
                    )
                };
                let basis_node = journal
                    .node(&basis)
                    .ok_or_else(|| Error::new("current Commons control basis is missing"))?;
                let current_binding = binds_local(basis_node);
                // A nonmember cannot decrypt the current roster. Public signed
                // binding evidence distinguishes that case before capsule access;
                // a failed decryption for a positively bound member stays an error.
                if !current_binding {
                    if journal
                        .inventory()
                        .filter_map(|id| journal.node(id))
                        .any(binds_local)
                    {
                        "membership-inactive"
                    } else {
                        "enrollment-pending"
                    }
                } else if let Some(access) = bootstrap
                    .access_in(&journal)
                    .map_err(crate::table::failure)?
                {
                    role = Some(match access.local().role() {
                        Role::Reader => "reader",
                        Role::Writer => "writer",
                    });
                    "ready"
                } else {
                    return Err(Error::new(
                        "current Commons binding has no usable membership",
                    ));
                }
            }
            ControlState::Empty | ControlState::Pending => "history-pending",
            ControlState::Uncertain => "control-uncertain",
            ControlState::Invalid { .. } => "control-invalid",
            ControlState::Conflict { .. } => "control-conflict",
        }
    };
    let trust = bootstrap.trust();
    let retained_nodes = journal.inventory().count();
    let largest_fact_count = journal
        .inventory()
        .filter_map(|id| journal.node(id))
        .map(|node| node.facts().len())
        .max();
    Ok(json!({
        "state": state, "ready": role.is_some(), "role": role,
        "sharing_enabled": sharing, "can_initialize_network": false,
        "network": "ALab Commons", "network_id": aterm_codec::hex::encode_lower(&trust.network),
        "authority": aterm_codec::hex::encode_lower(&trust.authority),
        "genesis": aterm_codec::hex::encode_lower(&trust.genesis),
        "actor": aterm_codec::hex::encode_lower(&request.binding().actor()),
        "control_inventory": {
            "retained_nodes": retained_nodes,
            "largest_fact_count": largest_fact_count,
            "fact_limit": MAX_FACTS,
            "legacy_fact_limit": LEGACY_MAX_FACTS,
        },
    }))
}

fn control_observation<T>(read: aterm_secrets::Result<T>) -> Result<Option<T>> {
    match read {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if matches!(
                error.kind(),
                aterm_secrets::ErrorKind::JournalBusy | aterm_secrets::ErrorKind::CustodyBusy
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(crate::table::failure(error)),
    }
}

fn control_busy(sharing: bool) -> Value {
    json!({
        "state": "control-busy",
        "detail": "Local Commons custody or control journal is busy; membership has not been checked",
        "membership_checked": false,
        "ready": false,
        "sharing_enabled": sharing,
        "can_initialize_network": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authority_inspection_never_initializes_an_absent_control_record() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("owner"));
        let config = root.path().join("config");
        let read = || run_at("commons-authority", &mut BTreeMap::new(), &paths, &config).unwrap();
        assert_eq!(read()["authority_available"].as_bool(), Some(false));
        assert!(!paths.home.exists());
        let source = root.path().join("vault");
        aterm_secrets::client::test_create_unbound(
            &paths,
            source.to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        let before = std::fs::read(&source).unwrap();
        let observed = read();
        assert_eq!(observed["authority_available"].as_bool(), Some(true));
        assert_eq!(observed["control"].as_str(), Some("absent"));
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
        assert_eq!(owner["can_initialize_network"].as_bool(), Some(false));
        let member = read(1);
        assert_eq!(member["authority_available"].as_bool(), Some(false));
        assert!(member.get("authority").is_none());
        assert!(member.get("descriptor").is_none());
        let invalid = run_at(
            "commons-authority",
            &mut BTreeMap::from([("--application", "ignored")]),
            &lab.paths[0],
            &lab.paths[0].home.join("config"),
        );
        assert!(invalid.is_err());
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
    fn retained_inventory_rejects_arguments_before_opening_custody() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("absent"));
        let valid = aterm_codec::hex::encode_lower(&[9; 32]);
        for mut options in [
            BTreeMap::new(),
            BTreeMap::from([("--table", "bad")]),
            BTreeMap::from([("--table", valid.as_str()), ("--unexpected", "value")]),
        ] {
            assert!(
                run_at(
                    "commons-inventory",
                    &mut options,
                    &paths,
                    &paths.home.join("config"),
                )
                .is_err()
            );
            assert!(!paths.home.exists());
        }
    }

    #[test]
    fn retained_inventory_does_not_create_an_absent_catalog_or_undeclared_table() {
        use aterm_secrets::commons::roster::Role;
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let bootstrap = lab.bootstrap(0);
        let directory = Directory::new(paths, bootstrap.trust());
        let config = paths.home.join("config");
        let catalog =
            inspect_inventory(paths, &config, directory.catalog_scope().digest()).unwrap();
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
        let catalog =
            inspect_inventory(paths, &config, directory.catalog_scope().digest()).unwrap();
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
        let first =
            Descriptor::application(bootstrap.trust(), "inventory-test", "rows", 1).unwrap();
        let second =
            Descriptor::application(bootstrap.trust(), "inventory-test", "rows", 2).unwrap();
        assert_eq!(first.scope, second.scope);
        let mut catalog = directory.catalog(&session).unwrap();
        for descriptor in [first.clone(), second] {
            catalog
                .commit_catalog(&session, &descriptor.encode().unwrap())
                .unwrap();
        }
        let read =
            inspect_inventory(paths, &paths.home.join("config"), first.scope.digest()).unwrap();
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
        std::fs::write(
            &config,
            format!("[commons]\nexclude_tables=[\"{encoded}\"]\n"),
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
            let ready = status(&lab.paths[index]);
            assert_eq!(ready["ready"].as_bool(), Some(true));
            assert_eq!(ready["role"].as_str(), Some(role));
            assert!(!lab.paths[index].dir.join("commons/tables").exists());
        }
        std::fs::write(
            lab.paths[0].home.join("config"),
            "[commons]\nenabled=false\n",
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
        assert_eq!(
            status(&pending.paths[1])["state"].as_str(),
            Some("enrollment-pending")
        );
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

    #[test]
    fn commons_status_beside_a_custody_observation_held_past_its_wait_is_busy() {
        use aterm_secrets::commons::roster::Role;
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let custody = aterm_uds::private_file::PrivateLock::take(
            &paths.dir,
            "commons.lock",
            std::time::Duration::ZERO,
            "held",
        )
        .unwrap();
        let busy = status(paths);
        assert_eq!(busy["state"].as_str(), Some("control-busy"), "{busy}");
        assert_eq!(busy["membership_checked"].as_bool(), Some(false));
        assert_eq!(busy["ready"].as_bool(), Some(false));
        assert!(busy.get("role").is_none());
        assert!(busy.get("network_id").is_none());
        drop(custody);
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
}
