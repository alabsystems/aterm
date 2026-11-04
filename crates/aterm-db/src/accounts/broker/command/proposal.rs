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
mod tests {
    use super::*;

    #[test]
    fn inspection_is_offline_and_bad_proposals_never_open_a_vault() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let file = dir.path().join("policy.toml");
        std::fs::write(&file, "version = 1\n").unwrap();
        let inspect = run_with(
            "accounts-broker-inspect",
            BTreeMap::from([("--file", file.to_str().unwrap())]),
            || panic!("offline inspection"),
        )
        .unwrap();
        assert_eq!(inspect["authority"], json!("none"));
        assert_eq!(inspect["grants"], json!([]));
        assert!(inspect.get("policy").is_none());
        let account = "31".repeat(32);
        let id = "61".repeat(16);
        let producer = "62".repeat(32);
        for (key, value) in [
            ("--seconds", "0"),
            ("--seconds", "3601"),
            ("--account", "sensitive-sentinel"),
            ("--grant", "sensitive-sentinel"),
            ("--producer", "sensitive-sentinel"),
            ("--unknown", "sensitive-sentinel"),
        ] {
            let mut options = BTreeMap::from([
                ("--account", account.as_str()),
                ("--grant", id.as_str()),
                ("--producer", producer.as_str()),
            ]);
            options.insert(key, value);
            let error = run_with("accounts-broker-propose", options, || {
                panic!("invalid input touched vault")
            })
            .unwrap_err();
            assert!(!error.to_string().contains("sensitive-sentinel"));
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "version = 1\n");
    }

    #[test]
    fn refresh_options_are_closed_before_opening_any_vault() {
        let id = "61".repeat(16);
        let zero = "00".repeat(16);
        for options in [
            BTreeMap::new(),
            BTreeMap::from([("--grant", "sensitive-sentinel")]),
            BTreeMap::from([("--grant", zero.as_str())]),
            BTreeMap::from([
                ("--grant", id.as_str()),
                ("--producer", "sensitive-sentinel"),
            ]),
            BTreeMap::from([("--grant", id.as_str()), ("--base", "sensitive-sentinel")]),
        ] {
            let error = run_with("accounts-broker-refresh", options, || {
                panic!("invalid refresh opened vault")
            })
            .unwrap_err();
            assert!(!error.to_string().contains("sensitive-sentinel"));
        }
    }

    #[test]
    fn publication_options_are_closed_before_opening_any_vault() {
        let id = "61".repeat(16);
        let base = "62".repeat(32);
        let zero_id = "00".repeat(16);
        let zero_base = "00".repeat(32);
        let valid = BTreeMap::from([
            ("--grant", id.as_str()),
            ("--base", base.as_str()),
            ("--file", "missing-policy.toml"),
        ]);
        for (key, value) in [
            ("--grant", "sensitive-sentinel"),
            ("--base", "sensitive-sentinel"),
            ("--grant", zero_id.as_str()),
            ("--base", zero_base.as_str()),
            ("--unknown", "sensitive-sentinel"),
        ] {
            let mut options = valid.clone();
            options.insert(key, value);
            let error = run_with("accounts-broker-publish", options, || {
                panic!("invalid publication opened vault")
            })
            .unwrap_err();
            assert!(!error.to_string().contains("sensitive-sentinel"));
        }
        for missing in ["--grant", "--base", "--file"] {
            let mut options = valid.clone();
            options.remove(missing);
            assert!(
                run_with("accounts-broker-publish", options, || panic!(
                    "incomplete publication opened vault"
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn extending_an_untrusted_base_never_replaces_a_producer_or_id() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let file = dir.path().join("base.toml");
        let grant = Grant {
            id: [61; 16],
            producer: [62; 32],
            actor: [63; 48],
            lifetime: Duration::from_secs(60),
            binding: crate::resources::RequestBinding {
                principal: [64; 32],
                account: [65; 32],
                revision: [66; 32],
                credential: [66; 32],
                policy: [67; 32],
                adapter: "example-v1".into(),
            },
        };
        let original = Policy::new(vec![grant]).unwrap().text();
        std::fs::write(&file, &original).unwrap();
        let account = "41".repeat(32);
        let id = "3d".repeat(16);
        let producer = "3e".repeat(32);
        let other_id = "4f".repeat(16);
        let other_producer = "50".repeat(32);
        for (id, producer) in [(&id, &other_producer), (&other_id, &producer)] {
            let options = BTreeMap::from([
                ("--account", account.as_str()),
                ("--grant", id.as_str()),
                ("--producer", producer.as_str()),
                ("--base", file.to_str().unwrap()),
            ]);
            assert!(
                run_with("accounts-broker-propose", options, || panic!(
                    "replacement touched vault"
                ))
                .is_err()
            );
        }
        let inspected = crate::cli::run(&[
            "accounts-broker-inspect".into(),
            "--file".into(),
            file.to_str().unwrap().into(),
        ])
        .unwrap();
        assert_eq!(inspected["grants"][0]["producer"], json!(producer));
        assert_eq!(std::fs::read_to_string(file).unwrap(), original);
    }
}
