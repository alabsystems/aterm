// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only billing policy tools. File proposals are untrusted data; only a
//! fresh root-pinned observation can bind them to an addressed credential.

use super::{Billing, ENTRY, Policy, Resource, policy};
use crate::{Error, Result, database::decode_hex, resources::Proposal};
use astream_wire::sync::Digest;
use aterm_codec::hex;
use aterm_json::{Value, json};
use aterm_secrets::{
    client::{self, Paths},
    fsx,
};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

pub(crate) fn run(command: &str, options: BTreeMap<&str, &str>) -> Result<Value> {
    run_with(command, options, || {
        Paths::live().map_err(crate::table::failure)
    })
}

fn run_with(
    command: &str,
    mut options: BTreeMap<&str, &str>,
    paths: impl FnOnce() -> Result<Paths>,
) -> Result<Value> {
    match command {
        "accounts-billing-propose" => {
            let wire = take(&mut options, "--allocation")?;
            let provider = take(&mut options, "--provider")?;
            let adapter = take(&mut options, "--adapter")?;
            let base = options.remove("--base");
            empty(&options)?;
            // Match the policy parser's allocation bound before allocating.
            if wire.len() > 8192 {
                return Err(Error::new("billing allocation exceeds input bound"));
            }
            let bytes = hex::decode_vec(wire.as_bytes())
                .ok_or_else(|| Error::new("invalid billing allocation hex"))?;
            let resource = Resource {
                allocation: Proposal::decode(&bytes)?,
                provider: provider.into(),
                adapter: adapter.into(),
            };
            resource.validate()?;
            let policy = match base {
                Some(path) => read_policy(path)?.with_resource(resource)?,
                None => Policy::new(vec![resource])?,
            };
            let mut result = describe(&policy, "billing-policy-proposal");
            result
                .as_object_mut()
                .unwrap()
                .insert("policy".into(), json!(policy.text()));
            Ok(result)
        }
        "accounts-billing-inspect" => {
            let file = take(&mut options, "--file")?;
            empty(&options)?;
            Ok(describe(&read_policy(file)?, "billing-policy-inspection"))
        }
        "accounts-billing-prepare" => {
            let account = decode_hex(take(&mut options, "--account")?)?;
            empty(&options)?;
            prepare_at(account, paths()?, Instant::now() + Duration::from_secs(2))
        }
        _ => Err(Error::new("unknown account billing command")),
    }
}

fn read_policy(path: &str) -> Result<Policy> {
    let bytes = fsx::read_public(Path::new(path), policy::MAX_BYTES)
        .map_err(|_| Error::new("billing policy file is unreadable or exceeds its bound"))?
        .ok_or_else(|| Error::new("billing policy file is missing"))?;
    Policy::parse(&bytes)
}

fn describe(policy: &Policy, kind: &str) -> Value {
    json!({
        "kind": kind, "authority": "none", "entry": ENTRY,
        "resources": policy.resources().map(|resource| {
            let terms = resource.allocation.terms();
            let trust = resource.allocation.trust();
            json!({
                "billing": policy.reference(terms.resource).unwrap().value(),
                "allocation": hex::encode_lower(&resource.allocation.fingerprint()),
                "provider": resource.provider, "adapter": resource.adapter,
                "proposed_root": { "network": hex::encode_lower(&trust.network),
                    "authority": hex::encode_lower(&trust.authority), "genesis": hex::encode_lower(&trust.genesis) },
                "window": hex::encode_lower(&terms.window), "unit": terms.unit, "limit": terms.limit,
                "shares": terms.shares.iter().map(|(actor, limit)| json!({
                    "actor": hex::encode_lower(actor), "limit": limit
                })).collect::<Vec<_>>()
            })
        }).collect::<Vec<_>>()
    })
}

pub(super) fn prepare_at(account: Digest, paths: Paths, deadline: Instant) -> Result<Value> {
    let config = client::config(&paths)
        .map_err(crate::table::failure)?
        .ok_or_else(|| Error::new("billing preparation needs a configured vault"))?;
    let prepared = Billing::at(paths, config, account).prepare_until(deadline)?;
    let binding = prepared.binding();
    Ok(json!({
        "kind": "billing-preparation-observation", "scope": "observation-only",
        "account": hex::encode_lower(&binding.account), "decision": hex::encode_lower(&binding.revision),
        "billing": super::Reference { principal: binding.principal, policy: binding.policy }.value(),
        "allocation": hex::encode_lower(&prepared.allocation().id()),
        "provider": prepared.preparation().configuration().account()["provider"],
        "adapter": binding.adapter, "credential_bound": true, "unit": "requests", "request_cost": 1,
        "ledger": "not-observed", "provider_request": "not-attempted"
    }))
}

fn take<'a>(options: &mut BTreeMap<&str, &'a str>, key: &str) -> Result<&'a str> {
    options
        .remove(key)
        .ok_or_else(|| Error::new("missing account billing option"))
}

fn empty(options: &BTreeMap<&str, &str>) -> Result<()> {
    if options.is_empty() {
        Ok(())
    } else {
        Err(Error::new("unknown account billing option"))
    }
}

#[cfg(test)]
mod tests;
