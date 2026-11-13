// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Untrusted proposal tools and explicit conditional root publication.
//! Proposal generation and inspection never publish or spend a resource.

use super::*;
use crate::{
    accounts::{
        billing::Billing,
        broker::{ENTRY, Grant, Policy, policy},
    },
    table::Session,
};
use astream_wire::sync::Digest;
use aterm_json::Value;
use aterm_secrets::commons::bootstrap::Bootstrap;

pub(crate) fn run(command: &str, options: BTreeMap<&str, &str>) -> Result<Value> {
    run_with(command, options, || {
        Paths::live().map_err(crate::table::failure)
    })
}

fn run_with(
    command: &str,
    options: BTreeMap<&str, &str>,
    paths: impl FnOnce() -> Result<Paths>,
) -> Result<Value> {
    run_until(
        command,
        options,
        paths,
        Instant::now()
            + Duration::from_secs(if command == "accounts-broker-publish" {
                60
            } else {
                2
            }),
    )
}

fn run_until(
    command: &str,
    mut options: BTreeMap<&str, &str>,
    paths: impl FnOnce() -> Result<Paths>,
    deadline: Instant,
) -> Result<Value> {
    match command {
        "accounts-broker-publish" => {
            let id = decode_hex(take(&mut options, "--grant")?)?;
            let base = decode_hex(take(&mut options, "--base")?)?;
            let file = take(&mut options, "--file")?;
            empty(&options)?;
            if id == [0; 16] || base == [0; 32] {
                return Err(Error::new("publication IDs must be nonzero"));
            }
            let target = read(file)?;
            let paths = paths()?;
            let config = client::config(&paths)
                .map_err(crate::table::failure)?
                .ok_or_else(|| Error::new("broker publication needs a configured vault"))?;
            let broker = Broker::at(paths.clone(), config)?;
            broker.check_publication_writer_until(id, deadline)?;
            let active = crate::application_paths_until(&paths, deadline)?;
            client::sync_reader_until(&active, deadline).map_err(crate::table::failure)?;
            let admin =
                client::admin_cached_until(&active, deadline).map_err(crate::table::failure)?;
            let result = broker.publish_refresh_until(id, base, &target, &admin, deadline)?;
            Ok(match result {
                super::super::Publication::AlreadyCurrent { sequence } => json!({
                    "kind": "broker-policy-publication", "status": "already-current", "sequence": sequence,
                    "provider_request": "not-attempted", "base_checked": false
                }),
                super::super::Publication::Published(published) => json!({
                    "kind": "broker-policy-publication", "status": "stored", "sequence": published.seq,
                    "queued": published.queued, "local": published.local, "scope": "receiving-stores",
                    "provider_request": "not-attempted"
                }),
            })
        }
        "accounts-broker-refresh" => {
            let id = decode_hex(take(&mut options, "--grant")?)?;
            empty(&options)?;
            if id == [0; 16] {
                return Err(Error::new("broker grant ID must be nonzero"));
            }
            let paths = paths()?;
            let config = client::config(&paths)
                .map_err(crate::table::failure)?
                .ok_or_else(|| Error::new("grant refresh needs a configured vault"))?;
            let refreshed = Broker::at(paths, config)?.refresh_proposal_until(id, deadline)?;
            Ok(describe_refresh(&refreshed))
        }
        "accounts-broker-inspect" => {
            let file = take(&mut options, "--file")?;
            empty(&options)?;
            Ok(describe(&read(file)?, "broker-policy-inspection"))
        }
        "accounts-broker-propose" => {
            let account = decode_hex(take(&mut options, "--account")?)?;
            let id = decode_hex(take(&mut options, "--grant")?)?;
            let producer = decode_hex(take(&mut options, "--producer")?)?;
            let seconds = options
                .remove("--seconds")
                .unwrap_or("60")
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=3600).contains(n))
                .ok_or_else(|| Error::new("--seconds must be 1 through 3600"))?;
            let base = options.remove("--base");
            empty(&options)?;
            if account == [0; 32] || id == [0; 16] || producer == [0; 32] {
                return Err(Error::new("broker proposal IDs must be nonzero"));
            }
            let base = base.map(read).transpose()?.unwrap_or(Policy::new(vec![])?);
            if base.grants().any(|g| g.id == id || g.producer == producer) {
                return Err(Error::new(
                    "broker proposal would replace an existing ID or producer",
                ));
            }
            let grant = propose_at(
                paths()?,
                account,
                id,
                producer,
                Duration::from_secs(seconds),
                deadline,
            )?;
            let policy = Policy::new(base.grants().cloned().chain([grant]).collect())?;
            let mut value = describe(&policy, "broker-policy-proposal");
            value
                .as_object_mut()
                .unwrap()
                .insert("policy".into(), json!(policy.text()));
            Ok(value)
        }
        _ => Err(Error::new("unknown broker policy command")),
    }
}

#[cfg(test)]
pub(crate) fn test_command_with_paths(
    command: &str,
    options: BTreeMap<&str, &str>,
    paths: Paths,
) -> Result<Value> {
    run_until(
        command,
        options,
        || Ok(paths),
        Instant::now() + Duration::from_secs(60),
    )
}

pub(crate) fn propose_at(
    paths: Paths,
    account: Digest,
    id: [u8; 16],
    producer: Digest,
    lifetime: Duration,
    deadline: Instant,
) -> Result<Grant> {
    let config = client::config(&paths)
        .map_err(crate::table::failure)?
        .ok_or_else(|| Error::new("broker proposal needs a configured vault"))?;
    let prepared = Billing::at(paths.clone(), config, account).prepare_until(deadline)?;
    let active = crate::application_paths_until(&paths, deadline)?;
    let bootstrap = Bootstrap::required_until(&active, deadline).map_err(crate::table::failure)?;
    let session = Session::open_until(&bootstrap, deadline)?;
    session.writer()?;
    let actor = session.actor();
    drop(session);
    let prepared = prepared.refresh_until(deadline)?;
    if crate::application_paths_until(&paths, deadline)? != active {
        return Err(Error::new("broker proposal custody changed"));
    }
    let grant = Grant {
        id,
        producer,
        actor,
        binding: prepared.binding().clone(),
        lifetime,
    };
    Policy::new(vec![grant.clone()])?;
    Ok(grant)
}

fn read(file: &str) -> Result<Policy> {
    let bytes = fsx::read_public(Path::new(file), policy::MAX_BYTES)
        .map_err(|_| Error::new("broker policy file is unreadable or exceeds its bound"))?
        .ok_or_else(|| Error::new("broker policy file is missing"))?;
    Policy::parse(&bytes)
}

fn describe(policy: &Policy, kind: &str) -> Value {
    json!({ "kind": kind, "authority": "none", "entry": ENTRY,
        "grants": policy.grants().map(|g| json!({
            "id": hex::encode_lower(&g.id), "producer": hex::encode_lower(&g.producer),
            "actor": hex::encode_lower(&g.actor), "account": hex::encode_lower(&g.binding.account),
            "principal": hex::encode_lower(&g.binding.principal), "revision": hex::encode_lower(&g.binding.revision),
            "credential": hex::encode_lower(&g.binding.credential), "policy": hex::encode_lower(&g.binding.policy),
            "adapter": g.binding.adapter, "lifetime_ms": g.lifetime.as_millis() as u64
        })).collect::<Vec<_>>() })
}

fn describe_refresh(refreshed: &super::super::Refresh) -> Value {
    let before = refreshed.before();
    let after = refreshed.after();
    let changes = [
        (
            "principal",
            before.binding.principal,
            after.binding.principal,
        ),
        ("revision", before.binding.revision, after.binding.revision),
        (
            "credential",
            before.binding.credential,
            after.binding.credential,
        ),
        ("policy", before.binding.policy, after.binding.policy),
    ]
    .into_iter()
    .filter(|(_, old, new)| old != new)
    .map(|(field, old, new)| {
        json!({
        "field": field, "before": hex::encode_lower(&old), "after": hex::encode_lower(&new)
        })
    })
    .collect::<Vec<_>>();
    let mut value = describe(refreshed.policy(), "broker-policy-refresh-proposal");
    let object = value.as_object_mut().unwrap();
    object.insert("grant".into(), json!(hex::encode_lower(&before.id)));
    object.insert(
        "base_policy_sha256".into(),
        json!(hex::encode_lower(&refreshed.base())),
    );
    object.insert("base_encoding".into(), json!("canonical-broker-policy-v1"));
    object.insert("changes".into(), json!(changes));
    object.insert("policy".into(), json!(refreshed.policy().text()));
    value
}

fn take<'a>(options: &mut BTreeMap<&str, &'a str>, key: &str) -> Result<&'a str> {
    options
        .remove(key)
        .ok_or_else(|| Error::new("missing broker policy option"))
}

fn empty(options: &BTreeMap<&str, &str>) -> Result<()> {
    if options.is_empty() {
        Ok(())
    } else {
        Err(Error::new("unknown broker policy option"))
    }
}

#[cfg(test)]
#[path = "proposal_tests.rs"]
mod tests;
