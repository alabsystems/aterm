// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::database::decode_hex;
use aterm_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn run(command: &str, options: BTreeMap<&str, &str>) -> Result<Value> {
    run_at(
        command,
        options,
        Paths::live().map_err(crate::table::failure)?,
    )
}
pub(super) fn run_at(
    command: &str,
    mut options: BTreeMap<&str, &str>,
    paths: Paths,
) -> Result<Value> {
    if !matches!(
        command,
        "resources-propose"
            | "resources-init"
            | "resources-status"
            | "resources-reserve"
            | "resources-attempt"
    ) {
        return Err(Error::new("unknown resource command"));
    }
    let proposed = if command == "resources-propose" {
        let text = take(&mut options, "--terms")?;
        if text.len() > 16 * 1024 {
            return Err(Error::new("resource proposal exceeds input bound"));
        }
        Some(terms(aterm_json::from_str(text).map_err(|_| {
            Error::new("invalid resource proposal JSON")
        })?)?)
    } else {
        None
    };
    let supplied = if proposed.is_none() {
        let wire = take(&mut options, "--allocation")?;
        if wire.len() > 2 * allocation::MAX_WIRE {
            return Err(Error::new("resource allocation exceeds input bound"));
        }
        let wire = hex::decode_vec(wire.as_bytes())
            .ok_or_else(|| Error::new("invalid resource allocation hex"))?;
        let pin = decode_hex(take(&mut options, "--pin")?)?;
        Some((wire, pin))
    } else {
        None
    };
    let reserve = if command == "resources-reserve" {
        Some((
            decode_hex(take(&mut options, "--request")?)?,
            take(&mut options, "--cost")?
                .parse::<u64>()
                .map_err(|_| Error::new("invalid resource cost"))?,
        ))
    } else {
        None
    };
    let attempt = if command == "resources-attempt" {
        Some(decode_hex::<16>(take(&mut options, "--request")?)?)
    } else {
        None
    };
    if !options.is_empty() {
        return Err(Error::new("unknown resource option"));
    }
    let bootstrap = Bootstrap::required(&paths).map_err(crate::table::failure)?;
    if let Some(terms) = proposed {
        let proposal = Proposal::prepare(bootstrap.trust(), terms)?;
        return Ok(json!({"kind":"resource-proposal","authority":"none",
            "allocation":hex::encode_lower(proposal.bytes()),"fingerprint":hex::encode_lower(&proposal.fingerprint())}));
    }
    let (wire, pin) = supplied.ok_or_else(|| Error::new("missing resource allocation"))?;
    let allocation = Allocation::verify(&wire, pin, bootstrap.trust())?;
    let resource = Resources::at(paths, allocation.clone());
    if let Some(request) = attempt {
        let status = resource.attempt(request)?;
        let attempt = status.map(|attempt| {
            let observation = attempt.observation.confirmed().map(|observation| json!({
                "outcome": match observation.outcome() { Outcome::Completed => "completed", Outcome::Rejected => "rejected" },
                "evidence": hex::encode_lower(&observation.evidence()),
            }));
            json!({
                "receipt_id": hex::encode_lower(&attempt.receipt.id),
                "operation": hex::encode_lower(&attempt.operation),
                "received_at_unix_ms": attempt.receipt.received_ms,
                "local_journal_offset": attempt.receipt.offset,
                "state": if observation.is_some() { "recorded" } else { "unknown" },
                "observation": observation,
            })
        });
        return Ok(json!({"kind":"resource-attempt","scope":"observation-only",
            "allocation":hex::encode_lower(&allocation.id()),"request":hex::encode_lower(&request),
            "attempt":attempt}));
    }
    if command == "resources-init" {
        let scope = resource.initialize()?;
        return Ok(
            json!({"kind":"resource-initialization","scope":"accounting-only",
                "table":hex::encode_lower(&scope.digest()),"allocation":hex::encode_lower(&allocation.id())}),
        );
    }
    if let Some((request, cost)) = reserve {
        let reservation = resource.reserve(request, cost)?;
        return Ok(
            json!({"kind":"resource-reservation","scope":"accounting-only",
            "allocation":hex::encode_lower(&allocation.id()),"receipt_id":hex::encode_lower(&reservation.receipt.id),
            "request":hex::encode_lower(&request),"fresh":reservation.fresh,"cost":cost,
            "received_at_unix_ms":reservation.receipt.received_ms,"local_journal_offset":reservation.receipt.offset}),
        );
    }
    if command != "resources-status" {
        return Err(Error::new("unknown resource command"));
    }
    let snapshot = resource.snapshot()?;
    Ok(
        json!({"kind":"resource-status","scope":"accounting-only","allocation":hex::encode_lower(&allocation.id()),
        "resource":hex::encode_lower(&allocation.terms().resource),"window":hex::encode_lower(&allocation.terms().window),
        "unit":allocation.terms().unit,"limit":allocation.terms().limit,"retained_records":snapshot.retained,
        "observation":"locally-retained","shares":allocation.terms().shares.iter().map(|(actor,limit)| {
            let used=snapshot.used.get(actor).copied().unwrap_or(0);
            json!({"actor":hex::encode_lower(actor),"limit":limit,"reserved_lower_bound":used,
                "unreserved_upper_bound":limit-used})
        }).collect::<Vec<_>>()}),
    )
}
fn take<'a>(options: &mut BTreeMap<&str, &'a str>, key: &str) -> Result<&'a str> {
    options
        .remove(key)
        .ok_or_else(|| Error::new("missing resource option"))
}
fn terms(value: Value) -> Result<Terms> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("resource terms must be an object"))?;
    let keys = ["resource", "window", "unit", "limit", "shares"];
    if object.len() != keys.len() || object.keys().any(|k| !keys.contains(&k.as_str())) {
        return Err(Error::new("unknown or missing resource term"));
    }
    let field = |name| {
        value
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("resource term must be text"))
    };
    let shares = value["shares"]
        .as_object()
        .ok_or_else(|| Error::new("resource shares must map actors to units"))?;
    if shares.len() > allocation::MAX_SHARES {
        return Err(Error::new("too many resource shares"));
    }
    Ok(Terms {
        resource: decode_hex(field("resource")?)?,
        window: decode_hex(field("window")?)?,
        unit: field("unit")?.into(),
        limit: value["limit"]
            .as_u64()
            .ok_or_else(|| Error::new("resource limit must be unsigned"))?,
        shares: shares
            .iter()
            .map(|(actor, share)| {
                Ok((
                    decode_hex(actor)?,
                    share
                        .as_u64()
                        .ok_or_else(|| Error::new("resource share must be unsigned"))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?,
    })
}
