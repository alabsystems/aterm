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
    if matches!(command, "commons-inventory" | "commons-custody") {
        let encoded = options
            .remove("--table")
            .ok_or_else(|| Error::new("missing --table"))?;
        let table = inspection_digest(encoded, "--table")?;
        let page = if command == "commons-inventory" {
            RecordPage::take(options)?
        } else {
            None
        };
        if !options.is_empty() {
            return Err(Error::new("unknown Commons inspection option"));
        }
        let deadline = crate::admission::deadline();
        if command == "commons-custody" {
            return custody_inspection(paths, config, table, deadline);
        }
        return retained_inventory(
            &crate::generation::observed_paths_until(paths, deadline)?,
            config,
            table,
            page,
            deadline,
        );
    }
    if command == "commons-status" {
        if !options.is_empty() {
            return Err(Error::new("unknown Commons table option"));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let recovery = crate::generation::status(paths, deadline)?;
        let chosen = crate::generation::observed_paths_until(paths, deadline);
        let mut value = readiness(chosen.as_ref().unwrap_or(paths), config, deadline)?;
        let history = membership_history(chosen.as_ref().unwrap_or(paths), deadline);
        value
            .as_object_mut()
            .ok_or_else(|| Error::new("invalid Commons status"))?
            .insert("membership_history".into(), history);
        if !recovery.is_null() {
            let fields = value
                .as_object_mut()
                .ok_or_else(|| Error::new("invalid Commons status"))?;
            fields.insert("writer_recovery".into(), recovery);
            if chosen.is_err() {
                fields.insert("ready".into(), Value::Bool(false));
                fields.insert("state".into(), Value::from("writer-recovery-pending"));
            }
        }
        return Ok(value);
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
        let version_raw = options.remove("--version").unwrap_or("1");
        if version_raw.is_empty() || !version_raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::new("invalid --version"));
        }
        let version = version_raw
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
    if command == "commons-authority" {
        return authority(paths);
    }
    if let Some((application, role, version, name)) = descriptor {
        let selected = crate::generation::selected(paths)?;
        let paths = &selected;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
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
    let deadline = crate::admission::deadline();
    let selected = crate::generation::observed_paths_until(paths, deadline)?;
    let paths = &selected;
    let bootstrap = observed_bootstrap(paths, deadline)?;
    let session = Session::observe_until(&bootstrap, deadline)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let policy = Policy::read(config)?;
    let snapshot = directory.observe_with_policy(&session, &policy)?;
    let rings = ring_tables(&session, &directory, &policy)?;
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
        "tables":entries, "rings":rings, "inert":snapshot.inert,
        "recovery_reader":snapshot.recovery, "writer_resume_authorized":false}),
    )
}

fn membership_history(paths: &Paths, deadline: std::time::Instant) -> Value {
    match aterm_secrets::client::inspect_membership_history_until(paths, deadline) {
        Ok(Some(history)) => json!({
            "state": "available", "scope": "authenticated-local-membership-history",
            "network_id": aterm_codec::hex::encode_lower(&history.network),
            "retained_records": history.retained_records(),
            "complete_records": history.complete.held(),
            "complete_inventory_sha256": aterm_codec::hex::encode_lower(&history.complete_inventory_sha256),
            "waiting_records": history.waiting,
            "complete_weighed_records": history.complete.weighed,
            "complete_keyed_records": history.complete.keyed,
            "remaining_records": history.remaining_records(),
            "record_bytes": history.bytes,
            "limits": {
                "held_records": history.budgets.held,
                "weighed_records": history.budgets.weighed,
                "keyed_records": history.budgets.keyed,
                "record_bytes": aterm_secrets::membership_log::MAX_RECORDS_STREAM,
            },
        }),
        Err(error) if error.kind() == aterm_secrets::ErrorKind::VaultBusy => {
            json!({"state": "busy"})
        }
        Ok(None) | Err(_) => json!({"state": "unavailable"}),
    }
}

fn custody_inspection(
    paths: &Paths,
    config: &Path,
    table: Digest,
    deadline: std::time::Instant,
) -> Result<Value> {
    let policy = Policy::read(config)?;
    let excluded = || {
        json!({
            "kind": "commons-custody", "scope": "local-recovery-evidence",
            "table": aterm_codec::hex::encode_lower(&table),
            "claim": "not-inspected", "directory": null, "receipt_prefix": null,
            "reason": if policy.enabled { "excluded" } else { "sharing-disabled" },
            "application_authenticated": false, "can_resume_writer": false,
        })
    };
    if !policy.enabled || policy.excluded.contains(&table) {
        return Ok(excluded());
    }
    let bootstrap = observed_bootstrap(paths, deadline)?;
    let directory = Directory::new(paths, bootstrap.trust());
    if !policy.includes(directory.catalog_scope().digest(), table) {
        return Ok(excluded());
    }
    let session = Session::observe_until(&bootstrap, deadline)?;
    Ok(crate::table::store::custody::inspect(&session, table)?.json())
}

/// Each ring's window tables, by period: their identity and how many operations this Mac retains
/// of each (none where it holds no storage for it).
/// A damaged state uses the clock's window, as repair readers do, without rebuilding the state.
/// Excluded windows remain listed, without opening their storage.
/// Recovery observations include authenticated primary data without restoring writer custody.
fn ring_tables(
    session: &Session<'_>,
    directory: &Directory,
    policy: &Policy,
) -> Result<Vec<Value>> {
    let now = aterm_time::unix_millis()
        .map(|ms| ms / 1000)
        .ok_or_else(|| Error::new("system clock cannot place the ring tables"))?;
    let mut tables = Vec::new();
    for ring in ring::RINGS {
        let state = directory.ring_view(session, ring);
        for epoch in state.window(ring, now) {
            let scope = ring.scope(directory.trust(), epoch)?;
            let included = policy.includes(directory.catalog_scope().digest(), scope.digest());
            let observation = if included {
                directory
                    .observe_table_if_exists(session, scope)?
                    .map(|store| {
                        store
                            .inventory(session)
                            .map(|records| (records.count(), store.is_recovery()))
                    })
                    .transpose()?
            } else {
                None
            };
            tables.push(json!({"application":ring.application, "role":ring.role,
                "period_s":ring.period_s, "epoch":epoch,
                "state":if included { "subscribed" } else { "excluded" },
                "table":aterm_codec::hex::encode_lower(&scope.digest()),
                "retained":observation.map(|(count, _)| count),
                "recovery_reader":observation.map(|(_, recovery)| recovery)}));
        }
    }
    Ok(tables)
}

fn inspection_digest(encoded: &str, option: &str) -> Result<Digest> {
    let digest = if encoded.len() == 64 {
        aterm_codec::hex::decode_vec(encoded.as_bytes()).and_then(|bytes| bytes.try_into().ok())
    } else {
        None
    };
    digest.ok_or_else(|| Error::new(&format!("invalid {option} digest")))
}

#[derive(Clone, Copy)]
struct RecordPage {
    limit: usize,
    after: Option<Digest>,
}

impl RecordPage {
    fn take(options: &mut BTreeMap<&str, &str>) -> Result<Option<Self>> {
        let limit = options.remove("--records");
        let after = options.remove("--after");
        let Some(limit) = limit else {
            return if after.is_some() {
                Err(Error::new("--after requires --records"))
            } else {
                Ok(None)
            };
        };
        if limit.is_empty() || !limit.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::new("--records must be an integer from 1 to 256"));
        }
        let limit = limit
            .parse::<usize>()
            .ok()
            .filter(|limit| (1..=256).contains(limit))
            .ok_or_else(|| Error::new("--records must be an integer from 1 to 256"))?;
        let after = after
            .map(|encoded| inspection_digest(encoded, "--after"))
            .transpose()?;
        Ok(Some(Self { limit, after }))
    }

    /// Public operation identifiers from this one read, without payloads or key material.
    fn value(self, inventories: Option<(&BTreeSet<Digest>, &BTreeSet<Digest>)>) -> Value {
        use std::ops::Bound::{Excluded, Unbounded};

        let (records, next_after) = match inventories {
            None => (None, None),
            Some((known, eligible)) => {
                let mut ids: Vec<_> = known
                    .range((self.after.map_or(Unbounded, Excluded), Unbounded))
                    .take(self.limit + 1)
                    .copied()
                    .collect();
                let next_after = if ids.len() > self.limit {
                    ids.truncate(self.limit);
                    ids.last().map(|id| aterm_codec::hex::encode_lower(id))
                } else {
                    None
                };
                let records: Vec<_> = ids
                    .iter()
                    .map(|id| {
                        json!({"operation":aterm_codec::hex::encode_lower(id),
                            "eligible":eligible.contains(id)})
                    })
                    .collect();
                (Some(records), next_after)
            }
        };
        json!({"limit":self.limit, "after":self.after.map(|id| aterm_codec::hex::encode_lower(&id)),
            "records":records, "next_after":next_after})
    }
}

fn retained_inventory(
    paths: &Paths,
    config: &Path,
    table: Digest,
    page: Option<RecordPage>,
    deadline: std::time::Instant,
) -> Result<Value> {
    retained_inventory_admitted(paths, config, table, page, deadline, || {})
}

fn retained_inventory_admitted(
    paths: &Paths,
    config: &Path,
    table: Digest,
    page: Option<RecordPage>,
    deadline: std::time::Instant,
    after_admission: impl FnOnce(),
) -> Result<Value> {
    use crate::replica::InventorySummary;

    // Keep the current and ancestor vault sources stable for this one local
    // query, from session admission through both verified inventory summaries.
    // No source guard escapes this function or crosses network I/O.
    let _sources = crate::generation::local_source_reads_until(paths, deadline)?;
    let bootstrap = observed_bootstrap(paths, deadline)?;
    let session = Session::observe_until(&bootstrap, deadline)?;
    after_admission();
    let directory = Directory::new(paths, bootstrap.trust());
    let catalog = directory.catalog_scope();
    let policy = Policy::read(config)?;
    let (subscription, scope) = if !policy.includes(catalog.digest(), table) {
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
    let reply = |storage: &str,
                 inventories: Option<(InventorySummary, InventorySummary)>,
                 recovery: bool|
     -> Result<Value> {
        let mut value = json!({
            "kind":"commons-inventory", "scope":"local-retained-operation-ids",
            "network_id":aterm_codec::hex::encode_lower(&bootstrap.trust().network),
            "table":aterm_codec::hex::encode_lower(&table), "subscription":subscription,
            "storage":storage, "known":inventories.map(|(known,_)|summary(known)),
            "eligible":inventories.map(|(_,eligible)|summary(eligible)),
            "recovery_reader":recovery, "writer_resume_authorized":false,
        });
        if let Some(page) = page {
            value
                .as_object_mut()
                .ok_or_else(|| Error::new("invalid inventory response"))?
                .insert("page".into(), page.value(None));
        }
        Ok(value)
    };
    let Some(scope) = scope else {
        return reply("not-observed", None, false);
    };
    let Some(store) = directory.observe_table_if_exists(&session, scope)? else {
        return reply("missing", None, false);
    };
    let known: BTreeSet<_> = store.inventory(&session)?.copied().collect();
    let eligible: BTreeSet<_> = store.replication_inventory(&session)?.copied().collect();
    let mut value = reply(
        "present",
        Some((
            InventorySummary::of(&known),
            InventorySummary::of(&eligible),
        )),
        store.is_recovery(),
    )?;
    if let Some(page) = page {
        value
            .as_object_mut()
            .ok_or_else(|| Error::new("invalid inventory response"))?
            .insert("page".into(), page.value(Some((&known, &eligible))));
    }
    Ok(value)
}

/// Preserve the vault's unavailable reason before requiring existing local custody.
fn observed_bootstrap(paths: &Paths, deadline: std::time::Instant) -> Result<Bootstrap> {
    if let aterm_secrets::commons::Observation::Unavailable(reason) =
        aterm_secrets::commons::observe_until(paths, deadline).map_err(crate::table::failure)?
    {
        return Err(
            if matches!(reason, aterm_secrets::commons::Unavailable::Busy) {
                Error::busy()
            } else {
                Error::new(&format!("{}: {}", reason.code(), reason.explanation()))
            },
        );
    }
    Bootstrap::established_with_journal_until(paths, deadline)
        .map(|(bootstrap, _)| bootstrap)
        .map_err(crate::table::failure)
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
    let audience = inspection.audience.as_ref().map(|audience| match audience {
        aterm_secrets::text::Audience::Everyone => json!({ "kind": "everyone" }),
        aterm_secrets::text::Audience::Hosts(hosts) => {
            json!({ "kind": "hosts", "hosts": hosts })
        }
    });
    Ok(json!({
        "kind": "commons-authority", "scope": "locally-applied-vault",
        "authority_available": true, "vault_version": inspection.vault_version,
        "authority": aterm_codec::hex::encode_lower(&inspection.authority),
        "lifetime_bound": inspection.lifetime_bound,
        "control": if inspection.descriptor.is_some() { "typed-descriptor" }
            else if inspection.entry_present { "opaque-entry" } else { "absent" },
        "descriptor": descriptor, "published_audience": audience, "can_initialize_network": false,
    }))
}

fn readiness(paths: &Paths, config: &Path, deadline: std::time::Instant) -> Result<Value> {
    use aterm_secrets::commons::{
        Observation,
        constraints::{Fact, LEGACY_MAX_FACTS, MAX_FACTS},
        journal::State as ControlState,
        roster::Role,
    };
    let sharing = Policy::read(config)?.enabled;
    let Some(observation) =
        control_observation(aterm_secrets::commons::observe_until(paths, deadline))?
    else {
        return Ok(control_busy(sharing));
    };
    let (bootstrap, journal) = match observation {
        Observation::Unavailable(reason) => {
            return Ok(json!({
                "state": reason.code(), "detail": reason.explanation(),
                    "ready": false, "sharing_enabled": sharing, "can_initialize_network": false,
            }));
        }
        Observation::Visible(_) => {
            let Some(pair) =
                control_observation(Bootstrap::established_with_journal_until(paths, deadline))?
            else {
                return Ok(control_busy(sharing));
            };
            pair
        }
    };
    // The acknowledged prefix, read beside any writer: no lock to find busy.
    bootstrap
        .revalidate_custody()
        .map_err(crate::table::failure)?;
    let request = bootstrap.request().map_err(crate::table::failure)?;
    let mut role = None;
    let state = if let Some((log, _)) =
        aterm_secrets::client::trusted_commons_log(bootstrap.paths(), bootstrap.trust())
            .map_err(crate::table::failure)?
    {
        use aterm_secrets::membership_log::{EpochAudience, NodeStanding};
        let actor = request.binding().actor();
        let recipient = request.binding().recipient();
        let log_roster = aterm_secrets::client::local_swarm_roster_for_log(bootstrap.paths(), &log)
            .map_err(crate::table::failure)?;
        let membership = log.membership();
        let member = membership.get(&actor);
        let excluded_in_log = membership.removal(&actor).is_some()
            || log.recipient_is_removed(&recipient)
            || matches!(
                membership.node(request.binding().writer()),
                NodeStanding::Removed(_)
            )
            // Audience checks deny unknown actors too. Only an existing seat
            // makes that denial evidence of inactive membership.
            || (member.is_some()
                && (log.audience_left_out(&EpochAudience::Commons, &actor)
                    || log.audience_left_out(&EpochAudience::Everyone, &actor)))
            || log.audience_left_out_recipient(&EpochAudience::Commons, &recipient)
            || log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
            || membership.members().iter().any(|(m_actor, m)| {
                (*m_actor == actor || m.binding.recipient() == recipient)
                    && (membership.member_node_removed(m)
                        || log_roster
                            .as_ref()
                            .is_some_and(|r| aterm_secrets::client::member_denied_by_roster(r, m)))
            })
            || aterm_secrets::client::local_recipient_denied_in_roster(bootstrap.paths());
        let active_member =
            !excluded_in_log && member.is_some_and(|m| &m.binding == request.binding());
        if !active_member {
            if excluded_in_log
                || log.has_historical_seat(request.binding())
                || log.has_known_actor(&actor)
            {
                "membership-inactive"
            } else if let Ok(Some(access)) = bootstrap.access_in(&journal) {
                role = Some(match access.local().role() {
                    Role::Reader => "reader",
                    Role::Writer => "writer",
                });
                "ready"
            } else {
                "enrollment-pending"
            }
        } else {
            match bootstrap.access_in(&journal) {
                Ok(Some(access)) => {
                    role = Some(match access.local().role() {
                        Role::Reader => "reader",
                        Role::Writer => "writer",
                    });
                    "ready"
                }
                Ok(None) if !log.wraps_maximal_audience_to(&EpochAudience::Commons, &recipient) => {
                    "history-pending"
                }
                Ok(None) | Err(_) => "membership-inactive",
            }
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
                let roster_denied =
                    aterm_secrets::client::local_recipient_denied_in_roster(bootstrap.paths());
                let current_binding = !roster_denied && binds_local(basis_node);
                // A nonmember cannot decrypt the current roster. Public signed
                // binding evidence distinguishes that case before capsule access;
                // a failed decryption for a positively bound member stays an error.
                if !current_binding {
                    if roster_denied
                        || journal
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
        Err(error) => {
            let error = crate::table::failure(error);
            if error.is_busy() {
                Ok(None)
            } else {
                Err(error)
            }
        }
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

    #[test]
    fn status_does_not_establish_visible_but_uninitialized_custody() {
        let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
        let paths = &lab.paths[0];
        let before = crate::tests::files(lab._home.path());
        assert!(
            run_at(
                "commons-status",
                &mut BTreeMap::new(),
                paths,
                &paths.home.join("config")
            )
            .is_err()
        );
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
        let read = retained_inventory_admitted(&selected, &config, scope.digest(), None, crate::admission::deadline(), || {
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
        })
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
                assert!(
                    run_at(command, &mut options, &paths, &paths.home.join("config"),).is_err()
                );
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
                let id =
                    inspection_digest(record["operation"].as_str().unwrap(), "operation").unwrap();
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
        let descriptor =
            Descriptor::application(bootstrap.trust(), "custody-test", "rows", 1).unwrap();
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
        let read = custody_inspection(paths, &config, scope.digest(), crate::admission::deadline())
            .unwrap();
        assert_eq!(read["claim"], json!("ready"));
        assert_eq!(read["receipt_prefix"]["state"], json!("verified"));
        assert_eq!(read["can_resume_writer"], json!(false));
        assert!(!read.to_string().contains("private-custody-payload"));
        std::fs::write(
            &config,
            format!(
                "[commons]\nexclude_tables = [\"{}\"]\n",
                aterm_codec::hex::encode_lower(&scope.digest())
            ),
        )
        .unwrap();
        // A damaged excluded target must remain uninspected.
        std::fs::write(
            directory.path(scope).unwrap().join("table.anchor"),
            b"damaged",
        )
        .unwrap();
        let excluded =
            custody_inspection(paths, &config, scope.digest(), crate::admission::deadline())
                .unwrap();
        assert_eq!(excluded["claim"], json!("not-inspected"));
        assert_eq!(excluded["reason"], json!("excluded"));
        assert_eq!(excluded["receipt_prefix"], Value::Null);
        std::fs::write(&config, "[commons]\nenabled = false\n").unwrap();
        let disabled =
            custody_inspection(paths, &config, scope.digest(), crate::admission::deadline())
                .unwrap();
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
            lab.bootstrap(index);
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

            aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600)
                .unwrap();
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

    #[test]
    fn commons_status_beside_a_custody_observation_held_past_its_wait_is_busy() {
        use aterm_secrets::commons::roster::Role;
        let lab = crate::table::tests::Lab::new(&[Role::Writer]);
        lab.bootstrap(0);
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

    #[test]
    fn commons_status_reports_membership_inactive_when_superseded_by_another_recipient_seat_in_log()
    {
        use aterm_secrets::{
            client::{admit_member_in_log, configure_log_member, load_records, save_records},
            commons::{bootstrap::Device, roster::Role},
            membership_log::{Entrant, Evidence},
        };
        let lab = crate::table::tests::Lab::with_foundation(&[Role::Writer, Role::Writer]);
        aterm_secrets::client::seed_vault_genesis(
            &lab.paths[0],
            &lab._home.path().join("genesis.v2"),
        )
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
        aterm_secrets::fsx::write_atomic(&m3_b.dir.join("identity"), &identity_bytes, 0o600)
            .unwrap();
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
            states == ["ready", "membership-inactive"]
                || states == ["membership-inactive", "ready"],
            "expected one ready and one membership-inactive seat, got: {status_a} / {status_b}"
        );
    }
}
