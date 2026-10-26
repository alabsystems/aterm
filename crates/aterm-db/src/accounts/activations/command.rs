// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_codec::hex::encode_lower as hex;
use aterm_json::{Value, json};

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
    let db = Activations::at(paths.clone());
    if command == "accounts-activation-init" {
        empty(&options)?;
        return Ok(json!({"table":hex(&db.initialize()?.digest()),"kind":"account-activations"}));
    }
    let account = crate::database::decode_hex(take(&mut options, "--account")?)?;
    if command == "accounts-activation-status" {
        empty(&options)?;
        return db.inspect(account);
    }
    if command == "accounts-prepare" {
        empty(&options)?;
        let config = aterm_secrets::client::config(&paths)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("account preparation needs a configured vault"))?;
        let prepared = db.prepare(account, &config)?;
        let Action::Prepare(p) = &prepared.decision().request().action else {
            unreachable!()
        };
        return Ok(
            json!({"kind":"account-preparation-observation","account":hex(&account),"decision":hex(&prepared.decision().id()),
            "provider":p.provider,"method":p.method,"credential_bound":prepared.credential().is_some(),"provider_login":"not-attempted"}),
        );
    }
    let nonce = crate::database::decode_hex(take(&mut options, "--request")?)?;
    let parents = take(&mut options, "--parents")?;
    let mut parents = if parents == "none" {
        vec![]
    } else {
        parents
            .split(',')
            .map(crate::database::decode_hex)
            .collect::<Result<Vec<_>>>()?
    };
    parents.sort();
    if parents.windows(2).any(|p| p[0] == p[1]) {
        return Err(Error::new("duplicate account parent"));
    }
    let key = if command == "accounts-authorize" {
        Some(take(&mut options, "--key")?.to_owned())
    } else {
        None
    };
    let actor = if command == "accounts-authorize" {
        Some(crate::database::decode_hex(take(&mut options, "--actor")?)?)
    } else {
        None
    };
    empty(&options)?;
    if !matches!(command, "accounts-authorize" | "accounts-stop") {
        return Err(Error::new("unknown account activation command"));
    }
    let admin = aterm_secrets::client::admin_open(&paths).map_err(crate::table::failure)?;
    let request = if let Some(key) = key {
        db.configuration(&admin, account, &key, actor.unwrap(), parents)?
    } else {
        db.stop_request(account, parents)?
    };
    let (decision, receipt) = db.issue(&admin, nonce, request)?;
    Ok(
        json!({"kind":"account-decision","account":hex(&account),"decision":hex(&decision.id()),"generation":decision.request().generation,
        "operation":hex(&receipt.id),"received_at_unix_ms":receipt.received_ms,"local_journal_offset":receipt.offset}),
    )
}

fn take<'a>(options: &mut BTreeMap<&str, &'a str>, name: &str) -> Result<&'a str> {
    options
        .remove(name)
        .ok_or_else(|| Error::new("missing account activation option"))
}
fn empty(options: &BTreeMap<&str, &str>) -> Result<()> {
    if options.is_empty() {
        Ok(())
    } else {
        Err(Error::new("unknown account activation option"))
    }
}
