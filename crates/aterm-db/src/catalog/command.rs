// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::documents::application::observed_session;
use aterm_json::{Value, json};
use aterm_secrets::commons::{bootstrap::Bootstrap, journal::Journal};

pub(crate) fn run(command: &str, mut options: BTreeMap<&str, &str>) -> Result<Value> {
    let paths = Paths::live().map_err(crate::table::failure)?;
    let config = aterm_types::dirs::aterm_config_path()
        .ok_or_else(|| Error::new("Commons configuration path is unavailable"))?;
    run_at(command, &mut options, &paths, &config)
}
pub(crate) fn run_at(
    command: &str,
    options: &mut BTreeMap<&str, &str>,
    paths: &Paths,
    config: &Path,
) -> Result<Value> {
    // Parse all arguments before consulting or creating local custody.
    if matches!(command, "commons-inventory" | "commons-custody") {
        let table = options.remove("--table");
        let batch = options.remove("--tables");
        let tables = match (table, batch) {
            (Some(encoded), None) => vec![inspection_digest(encoded, "--table")?],
            (None, Some(encoded)) if command == "commons-inventory" => {
                let mut tables = Vec::new();
                for encoded in encoded.split(',') {
                    let table = inspection_digest(encoded, "--tables")?;
                    if tables.len() == MAX_TABLES + 1 || tables.contains(&table) {
                        return Err(Error::new("--tables requires 1 to 64 distinct digests"));
                    }
                    tables.push(table);
                }
                tables
            }
            _ => return Err(Error::new("specify exactly one of --table or --tables")),
        };
        let page = if command == "commons-inventory" {
            RecordPage::take(options)?
        } else {
            None
        };
        if !options.is_empty() {
            return Err(Error::new("unknown Commons inspection option"));
        }
        if batch.is_some() && page.is_some() {
            return Err(Error::new("record pages require a single --table"));
        }
        let deadline = crate::admission::deadline();
        if command == "commons-custody" {
            return custody_inspection(paths, config, tables[0], deadline);
        }
        let selected = crate::generation::completed_generation_until(paths, deadline)?;
        if batch.is_some() {
            let results =
                retained_inventories_admitted(&selected, config, &tables, None, deadline, || {})?;
            let ok = results.iter().all(Result::is_ok);
            let results: Vec<_> = tables
                .into_iter()
                .zip(results)
                .map(|(table, result)| {
                    result.unwrap_or_else(|error| {
                        json!({
                            "table":aterm_codec::hex::encode_lower(&table),
                            "storage":"unavailable", "known":null, "eligible":null,
                            "error":error.to_string(),
                        })
                    })
                })
                .collect();
            return Ok(json!({"kind":"commons-inventory-batch", "ok":ok,
                "scope":"local-retained-operation-ids", "tables":results,
                "atomic":false, "writer_resume_authorized":false}));
        }
        return retained_inventory(&selected, config, tables[0], page, deadline);
    }
    if command == "commons-status" {
        if !options.is_empty() {
            return Err(Error::new("unknown Commons table option"));
        }
        return commons_status_until(
            paths,
            config,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        );
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
    let selected = crate::generation::observed_generation_until(paths, deadline)?;
    let paths = &selected;
    let (bootstrap, journal) = observed_bootstrap(paths, deadline)?;
    let session = observed_session(&bootstrap, journal, deadline, &|| deadline)?;
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

/// `aterm db commons-status` by `deadline`, which bounds its writer-recovery report and the
/// selection it makes, its readiness and its membership history; its repairs read takes none.
pub(crate) fn commons_status_until(
    paths: &Paths,
    config: &Path,
    deadline: std::time::Instant,
) -> Result<Value> {
    let (recovery, chosen) = crate::generation::status(paths, deadline)?;
    let mut value = readiness(chosen.as_ref().unwrap_or(paths), config, deadline)?;
    let repairs = match crate::network_state::read_live_observation(config) {
        Ok(observation) => crate::network_state::value(&observation),
        Err(_) => match crate::network_state::read_history(config) {
            Ok(observation) => crate::network_state::value(&observation),
            Err(error) => {
                json!({"visibility":"Unavailable", "peers":[], "cause":error.repair_cause().word()})
            }
        },
    };
    value
        .as_object_mut()
        .ok_or_else(|| Error::new("invalid Commons status"))?
        .insert("repairs".into(), repairs);
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
    Ok(value)
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
    let (bootstrap, journal) = observed_bootstrap(paths, deadline)?;
    let directory = Directory::new(paths, bootstrap.trust());
    if !policy.includes(directory.catalog_scope().digest(), table) {
        return Ok(excluded());
    }
    let session = Session::from_observed_journal(&bootstrap, journal, Some(deadline))?;
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
    let catalog_digest = directory.catalog_scope().digest();
    let mut tables = Vec::new();
    for ring in ring::RINGS {
        let state = directory.ring_view(session, ring);
        for epoch in state.window(ring, now) {
            let scope = ring.scope(directory.trust(), epoch)?;
            let digest = scope.digest();
            let included = policy.includes(catalog_digest, digest);
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
                "table":aterm_codec::hex::encode_lower(&digest),
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
                let mut iter = known.range((self.after.map_or(Unbounded, Excluded), Unbounded));
                let records: Vec<_> = iter
                    .by_ref()
                    .take(self.limit)
                    .map(|id| {
                        json!({"operation":aterm_codec::hex::encode_lower(id),
                            "eligible":eligible.contains(id)})
                    })
                    .collect();
                let next_after = if iter.next().is_some() {
                    records
                        .last()
                        .and_then(|r| r.get("operation"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                } else {
                    None
                };
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
    retained_inventories_admitted(paths, config, &[table], page, deadline, after_admission)?
        .pop()
        .ok_or_else(|| Error::new("inventory response is missing"))?
}

/// Reuse one authenticated custody and policy observation across bounded local
/// reads. Individual table failures remain failures, never empty inventories.
fn retained_inventories_admitted(
    paths: &Paths,
    config: &Path,
    tables: &[Digest],
    page: Option<RecordPage>,
    deadline: std::time::Instant,
    after_admission: impl FnOnce(),
) -> Result<Vec<Result<Value>>> {
    // Keep the current and ancestor vault sources stable for this one local
    // query, from session admission through both verified inventory summaries.
    // No source guard escapes this function or crosses network I/O.
    let _sources = crate::generation::local_source_reads_until(paths, deadline)?;
    let (bootstrap, journal) = observed_bootstrap(paths, deadline)?;
    let session = observed_session(&bootstrap, journal, deadline, &|| deadline)?;
    after_admission();
    let directory = Directory::new(paths, bootstrap.trust());
    let policy = Policy::read(config)?;
    let now = aterm_time::unix_millis().map(|ms| ms / 1000);
    let mut snapshot = None;
    // Resolve all scopes before reading any table. A failed common catalog
    // observation cannot be mistaken for successful partial enumeration.
    let targets = tables
        .iter()
        .map(|&table| {
            inventory_scope(&session, &directory, &policy, table, &mut snapshot, now)
                .map(|(subscription, scope)| (table, subscription, scope))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(targets
        .into_iter()
        .map(|(table, subscription, scope)| {
            inventory_value(&session, &directory, table, subscription, scope, page)
        })
        .collect())
}

fn inventory_scope(
    session: &Session<'_>,
    directory: &Directory,
    policy: &Policy,
    table: Digest,
    snapshot: &mut Option<Snapshot>,
    now: Option<u64>,
) -> Result<(&'static str, Option<Scope>)> {
    let catalog = directory.catalog_scope();
    let catalog_digest = catalog.digest();
    let (subscription, scope) = if !policy.includes(catalog_digest, table) {
        ("excluded", None)
    } else if table == catalog_digest {
        ("catalog", Some(catalog))
    } else if let Some((_, _, scope)) = directory.ring_of(
        session,
        table,
        now.ok_or_else(|| Error::new("system clock cannot place the ring tables"))?,
    )? {
        ("ring", Some(scope))
    } else {
        if snapshot.is_none() {
            *snapshot = Some(directory.observe_with_policy(session, policy)?);
        }
        match snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.entries.get(&table))
        {
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
    Ok((subscription, scope))
}

fn inventory_value(
    session: &Session<'_>,
    directory: &Directory,
    table: Digest,
    subscription: &str,
    scope: Option<Scope>,
    page: Option<RecordPage>,
) -> Result<Value> {
    use crate::replica::InventorySummary;

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
            "network_id":aterm_codec::hex::encode_lower(&directory.trust().network),
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
    let Some(store) = directory.observe_table_if_exists(session, scope)? else {
        return reply("missing", None, false);
    };
    let known: BTreeSet<_> = store.inventory(session)?.copied().collect();
    let eligible: BTreeSet<_> = store.replication_inventory(session)?.copied().collect();
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
fn observed_bootstrap(paths: &Paths, deadline: std::time::Instant) -> Result<(Bootstrap, Journal)> {
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
    Bootstrap::established_with_journal_until(paths, deadline).map_err(crate::table::failure)
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
            if !aterm_secrets::commons::bootstrap::custody_opened(paths)
                .map_err(crate::table::failure)?
            {
                return Ok(custody_pending(sharing));
            }
            let Some(pair) =
                control_observation(Bootstrap::established_with_journal_until(paths, deadline))?
            else {
                return Ok(control_busy(sharing));
            };
            pair
        }
    };
    // A vault writer can take this Mac's state lock between the reads from here on: each read
    // that meets one says busy, never a membership verdict it did not judge.
    #[cfg(test)]
    tests::reached(tests::Point::Custody);
    if control_observation(bootstrap.revalidate_custody())?.is_none() {
        return Ok(control_busy(sharing));
    }
    let request = bootstrap.request().map_err(crate::table::failure)?;
    let mut role = None;
    let state = if let Some((log, heads)) =
        aterm_secrets::client::trusted_commons_log(bootstrap.paths(), bootstrap.trust())
            .map_err(crate::table::failure)?
    {
        use aterm_secrets::membership_log::EpochAudience;
        let actor = request.binding().actor();
        let recipient = request.binding().recipient();
        #[cfg(test)]
        tests::reached(tests::Point::Roster);
        let log_roster = match control_observation(
            aterm_secrets::client::local_swarm_roster_for_log(bootstrap.paths(), &log),
        ) {
            Ok(Some(log_roster)) => log_roster,
            Ok(None) => return Ok(control_busy(sharing)),
            // A vault writer that finished after `log` was read leaves it behind the history the
            // roster read requires: the log moved, a writer's moment, not this read's failure.
            Err(_) if log_moved(&bootstrap, &heads) => return Ok(control_busy(sharing)),
            Err(error) => return Err(error),
        };
        let membership = log.membership_ref();
        let member = membership.get(&actor);
        // The one removal predicate first; the seat scan below asks more of the seats that share
        // the actor or recipient (a removed writer key, a roster denial) than its kin verdict does.
        let excluded_in_log = log.seat_removed(request.binding())
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
            || match local_denial(bootstrap.paths()) {
                Some(denied) => denied,
                None => return Ok(control_busy(sharing)),
            };
        let active_member =
            !excluded_in_log && member.is_some_and(|m| &m.binding == request.binding());
        if !active_member {
            if excluded_in_log
                || log.has_historical_seat(request.binding())
                || log.has_known_actor(&actor)
            {
                "membership-inactive"
            } else {
                match access_now(&bootstrap, &journal) {
                    Ok(Some(Some(access))) => {
                        role = Some(match access.local().role() {
                            Role::Reader => "reader",
                            Role::Writer => "writer",
                        });
                        "ready"
                    }
                    Ok(None) => return Ok(control_busy(sharing)),
                    Ok(Some(None)) | Err(_) => "enrollment-pending",
                }
            }
        } else {
            match access_now(&bootstrap, &journal) {
                Ok(None) => return Ok(control_busy(sharing)),
                Ok(Some(Some(access))) => {
                    role = Some(match access.local().role() {
                        Role::Reader => "reader",
                        Role::Writer => "writer",
                    });
                    "ready"
                }
                Ok(Some(None))
                    if !log.wraps_maximal_audience_to(&EpochAudience::Commons, &recipient) =>
                {
                    "history-pending"
                }
                Ok(Some(None)) | Err(_) => "membership-inactive",
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
                let Some(roster_denied) = local_denial(bootstrap.paths()) else {
                    return Ok(control_busy(sharing));
                };
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
                } else if let Some(access) = match access_now(&bootstrap, &journal)? {
                    Some(access) => access,
                    None => return Ok(control_busy(sharing)),
                } {
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
    let mut retained_nodes = 0usize;
    let mut largest_fact_count = None;
    for id in journal.inventory() {
        retained_nodes += 1;
        if let Some(node) = journal.node(id) {
            let count = node.facts().len();
            largest_fact_count =
                Some(largest_fact_count.map_or(count, |max: usize| max.max(count)));
        }
    }
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

/// Whether this Mac's vault denies it, as `local_recipient_denied_in_roster` judges it (a state
/// that cannot be judged denies), except that a vault writer at work is `None`: nothing judged,
/// which readiness reports as busy ([`control_busy`]).
fn local_denial(paths: &Paths) -> Option<bool> {
    #[cfg(test)]
    tests::reached(tests::Point::Denial);
    match control_observation(aterm_secrets::client::local_recipient_denial_observed(
        paths,
    )) {
        Ok(Some(denied)) => Some(denied),
        Ok(None) => None,
        Err(_) => Some(true),
    }
}

/// Whether this Mac's trusted log is no longer the one whose heads digest is `heads`, which
/// readiness read before its roster: a vault writer finished, or is at work, in between. A read
/// that cannot say is `false`, so the roster read's own error stands.
fn log_moved(bootstrap: &Bootstrap, heads: &[u8; 32]) -> bool {
    match control_observation(aterm_secrets::client::trusted_commons_log(
        bootstrap.paths(),
        bootstrap.trust(),
    )) {
        Ok(Some(Some((_, now)))) => now != *heads,
        Ok(Some(None) | None) => true,
        Err(_) => false,
    }
}

/// `bootstrap`'s access through `journal` as this Mac's vault holds it now, `None` where a vault
/// writer is at work ([`control_observation`]): nothing judged, which readiness reports as busy.
///
/// # Errors
/// The access cannot be read for any other reason.
fn access_now(
    bootstrap: &Bootstrap,
    journal: &Journal,
) -> Result<Option<Option<aterm_secrets::commons::roster::Access>>> {
    #[cfg(test)]
    tests::reached(tests::Point::Access);
    control_observation(bootstrap.access_observed_in(journal))
}

/// Readiness on a Mac whose vault names Commons but which has opened no Commons custody yet
/// ([`aterm_secrets::commons::bootstrap::custody_opened`]): there is nothing to judge membership
/// by until aterm's next Commons open here makes it, and the read creates none.
fn custody_pending(sharing: bool) -> Value {
    json!({
        "state": "custody-pending",
        "detail": "This Mac has opened no Commons custody yet: an aterm window, `aterm node serve` or the Commons runtime opens it; membership has not been checked",
        "membership_checked": false,
        "ready": false,
        "sharing_enabled": sharing,
        "can_initialize_network": false,
    })
}

fn control_busy(sharing: bool) -> Value {
    json!({
        "state": "control-busy",
        "detail": "Local Commons custody, control journal or vault is busy; membership has not been checked",
        "membership_checked": false,
        "ready": false,
        "sharing_enabled": sharing,
        "can_initialize_network": false,
    })
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
