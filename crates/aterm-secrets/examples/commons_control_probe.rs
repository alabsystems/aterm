// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read local Commons visibility and membership-log capacity under one vault
//! observation. No network, identity creation, publication or credential output.

#[cfg(unix)]
fn stored_stream(
    paths: &aterm_secrets::client::Paths,
    store: &aterm_secrets::membership_log::Store,
) -> Result<aterm_json::Value, Box<dyn std::error::Error>> {
    use aterm_json::json;
    use aterm_secrets::{
        fsx,
        membership_log::{MAX_RECORDS, MAX_RECORDS_STREAM, decode_records},
    };

    let Some(bytes) = fsx::read_private(&paths.records_file(), MAX_RECORDS_STREAM)? else {
        return Ok(aterm_json::Value::Null);
    };
    let records = match decode_records(bytes.as_slice()) {
        Ok(records) => records,
        Err(error) => {
            return Ok(json!({
                "bytes": bytes.as_slice().len(), "decoded": false,
                "error": error.to_string(),
            }));
        }
    };
    // Recovery may skip individually signed records whose parent/seat checks
    // fail. Recheck only those records in memory; never repair the on-disk log.
    let mut replay = store.clone();
    let mut missing = 0;
    let mut accepted = 0;
    let mut errors = std::collections::BTreeMap::<String, usize>::new();
    for record in &records {
        if store.contains(record.id()) {
            continue;
        }
        missing += 1;
        match replay.insert(record.clone()) {
            Ok(true) => accepted += 1,
            Ok(false) => {}
            Err(error) => *errors.entry(error.to_string()).or_default() += 1,
        }
    }
    Ok(json!({
        "bytes": bytes.as_slice().len(), "decoded": true,
        "records": records.len(), "at_record_limit": records.len() >= MAX_RECORDS,
        "missing_from_recovered": missing, "accepted_in_memory": accepted,
        "recheck_errors": errors,
    }))
}

#[cfg(unix)]
fn observe(
    paths: &aterm_secrets::client::Paths,
) -> Result<aterm_json::Value, Box<dyn std::error::Error>> {
    use aterm_json::json;
    use aterm_secrets::{
        client,
        commons::{self, Observation},
        membership_log::{Body, EpochAudience, MAX_RECORDS},
    };

    let Some(_guard) = client::try_commons_source_read(paths)? else {
        return Ok(json!({
            "schema": 1, "kind": "commons-control-evidence",
            "scope": "local-vault-observation", "available": false,
            "reason": "vault observation lock absent or busy",
            "grants_membership": false,
        }));
    };
    let identity = client::read_machine_identity(paths)?;
    let observation = match commons::observe(paths)? {
        Observation::Visible(snapshot) => json!({
            "visible": true, "vault_sequence": snapshot.sequence(),
        }),
        Observation::Unavailable(reason) => json!({
            "visible": false, "reason": reason.code(),
        }),
    };
    let store = client::load_records(paths)?;
    let stored_stream = stored_stream(paths, &store)?;
    let log = match store.log() {
        None => aterm_json::Value::Null,
        Some(log) => {
            let membership = log.membership();
            let proof_capacity = client::ProofAdmissionCapacity::observe(log);
            let opened = identity
                .as_ref()
                .map(|identity| log.open_audience_entries(identity))
                .transpose()?
                .flatten();
            let recipient = identity.as_ref().map(|identity| identity.recipient());
            let epochs = opened
                .as_ref()
                .and_then(|opened| opened.maximal_epochs.get(&EpochAudience::Commons))
                .into_iter()
                .flatten()
                .map(|id| {
                    let record = log.get(id);
                    let author = record.and_then(|record| membership.members().get(record.actor()));
                    let generation = log.get(id).and_then(|record| match record.body() {
                        Body::Epoch { generation, .. } => Some(*generation),
                        _ => None,
                    });
                    let wraps = opened.as_ref().and_then(|opened| opened.wrapped_recipients.get(id));
                    let raw_wraps = log.records().filter_map(|record| match record.body() {
                        Body::Wrap { epoch, recipient, .. } if epoch == id => Some(*recipient),
                        _ => None,
                    }).collect::<std::collections::BTreeSet<_>>();
                    json!({
                        "id": aterm_codec::hex::encode_lower(id), "generation": generation,
                        "author_label": author.map(|member| member.label.as_str()),
                        "authored_ms": record.map(|record| record.authored_ms()),
                        "position": log.record_index(id),
                        "wrapped_recipients": wraps.map_or(0, |recipients| recipients.len()),
                        "local_wrap": recipient.map(|recipient| wraps.is_some_and(|wraps| wraps.contains(&recipient))),
                        "raw_wrapped_recipients": raw_wraps.len(),
                        "raw_local_wrap": recipient.map(|recipient| raw_wraps.contains(&recipient)),
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "complete_records": log.held_count(), "waiting_records": log.waiting(),
                "active_members": membership.members().len(),
                "new_proof_capacity": {
                    "scope": "new-proof-admissions-only",
                    "complete_records": proof_capacity.complete_records,
                    "exclusive_record_limit": proof_capacity.exclusive_record_limit,
                    "within_limit": proof_capacity.within_limit,
                    "grants_membership": false,
                },
                "local_recipient_removed": recipient.map(|recipient| log.recipient_is_removed(&recipient)),
                "local_everyone_excluded": recipient.map(|recipient| log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)),
                "local_commons_excluded": recipient.map(|recipient| log.audience_left_out_recipient(&EpochAudience::Commons, &recipient)),
                "opened_commons_control": opened.as_ref().map(|opened| opened.commons_control.is_some()),
                "maximal_commons_epochs": epochs,
            })
        }
    };
    Ok(json!({
        "schema": 1, "kind": "commons-control-evidence",
        "scope": "local-vault-observation", "available": true,
        "identity_present": identity.is_some(), "control": observation,
        "retained_records": store.len(), "record_limit": MAX_RECORDS,
        "at_record_limit": store.len() >= MAX_RECORDS, "log": log, "stored_stream": stored_stream,
        "grants_membership": false,
    }))
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().len() != 1 {
        return Err("usage: commons_control_probe".into());
    }
    println!("{}", observe(&aterm_secrets::client::Paths::live()?)?);
    Ok(())
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("commons_control_probe requires Unix vault support");
    std::process::ExitCode::from(2)
}

#[cfg(all(test, unix))]
mod tests {
    use super::observe;
    use aterm_secrets::{
        client::{self, Paths},
        text::Audience,
    };

    #[test]
    fn missing_state_is_observed_without_creating_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("absent");
        let value = observe(&Paths::for_home(&home)).unwrap();
        assert_eq!(value["available"], aterm_json::json!(false));
        assert!(!home.exists());
    }

    #[test]
    fn authenticated_observation_never_serializes_credentials() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        client::create(
            &paths,
            root.path().join("vault").to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        let admin = client::admin_cached(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            "private-diagnostic-canary",
            b"private-diagnostic-payload",
        )
        .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let before = std::fs::read(paths.records_file()).ok();
        let value = observe(&paths).unwrap();
        let after = std::fs::read(paths.records_file()).ok();
        assert_eq!(before, after);
        assert_eq!(value["available"], aterm_json::json!(true));
        assert_eq!(value["identity_present"], aterm_json::json!(true));
        assert_eq!(value["control"]["visible"], aterm_json::json!(true));
        let text = value.to_string();
        assert!(!text.contains("private-diagnostic-"));
        assert!(!text.contains("AGE-SECRET-KEY-"));
    }
}
