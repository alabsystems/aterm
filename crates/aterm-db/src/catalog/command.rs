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
    let bootstrap = Bootstrap::required(paths).map_err(crate::table::failure)?;
    let session = Session::open(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    if let Some((application, role, version, name)) = descriptor {
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
    let snapshot = directory.snapshot_with_config(&session, config)?;
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
        "tables":entries, "inert":snapshot.inert}),
    )
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
        bootstrap::Availability, constraints::Fact, journal::State as ControlState, roster::Role,
    };
    let sharing = Policy::read(config)?.enabled;
    let bootstrap = match Bootstrap::observe(paths).map_err(crate::table::failure)? {
        Availability::Unavailable(reason) => {
            return Ok(json!({
                "state": reason.code(), "detail": reason.explanation(),
                    "ready": false, "sharing_enabled": sharing, "can_initialize_network": false,
            }));
        }
        Availability::Available(bootstrap) => bootstrap,
    };
    let journal = aterm_secrets::commons::journal::Journal::open_existing(
        bootstrap.journal_dir(),
        bootstrap.trust(),
    )
    .map_err(crate::table::failure)?;
    let request = bootstrap.request().map_err(crate::table::failure)?;
    let mut role = None;
    let state = match journal.state() {
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
    };
    let trust = bootstrap.trust();
    Ok(json!({
        "state": state, "ready": role.is_some(), "role": role,
        "sharing_enabled": sharing, "can_initialize_network": false,
        "network": "ALab Commons", "network_id": aterm_codec::hex::encode_lower(&trust.network),
        "authority": aterm_codec::hex::encode_lower(&trust.authority),
        "genesis": aterm_codec::hex::encode_lower(&trust.genesis),
        "actor": aterm_codec::hex::encode_lower(&request.binding().actor()),
    }))
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
        assert!(
            run_at(
                "commons-tables",
                &mut BTreeMap::new(),
                &paths,
                &paths.home.join("config"),
            )
            .is_ok()
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
