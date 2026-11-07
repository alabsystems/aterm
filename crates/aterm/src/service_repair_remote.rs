// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Client composition for bounded service repair over the native fleet Drive.

use aterm_json::Value;
use aterm_secrets::service_repair as transaction;
use std::path::Path;

const RESULT_DOMAIN: &str = "aterm-service-rpc-result/1";
const REQUEST_DOMAIN: &str = "aterm-service-activation/1";

fn option<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    let mut matches = args
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|pair| pair[0] == name);
    let value = matches.next().ok_or_else(|| format!("missing {name}"))?;
    if matches.next().is_some() {
        return Err(format!("duplicate {name}"));
    }
    Ok(&value[1])
}
fn options(args: &[String], allowed: &[&str]) -> Result<(), String> {
    if !args.len().is_multiple_of(2)
        || args
            .as_chunks::<2>()
            .0
            .iter()
            .any(|pair| !allowed.contains(&pair[0].as_str()))
    {
        return Err("invalid remote service repair options".into());
    }
    for name in allowed {
        option(args, name)?;
    }
    Ok(())
}
fn identity() -> Result<aterm_net::fleet::NodeIdentity, String> {
    let state = aterm_types::dirs::state_dir().ok_or("no fleet state directory")?;
    aterm_net::fleet_member::read_node_identity(&state)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "remote service repair requires existing fleet identity".into())
}
fn decoded_signature(value: &Value) -> Result<[u8; 64], String> {
    let text = value["signature"]
        .as_str()
        .ok_or("missing signed reply signature")?;
    let mut signature = [0; 64];
    if !aterm_codec::hex::decode_into(text.as_bytes(), &mut signature) {
        return Err("invalid signed reply signature".into());
    }
    Ok(signature)
}
/// Verify the remote result signature in addition to the authenticated Drive.
/// The Drive dial binds the target node to its current fleet key before any request.
fn reply_payload(reply: &Value) -> Result<&Value, String> {
    let key = reply["key"]
        .as_str()
        .and_then(aterm_net::fleet::NodeKey::from_hex)
        .ok_or("invalid signed reply key")?;
    if reply["node"].as_str().is_none_or(str::is_empty) {
        return Err("missing signed reply node".into());
    }
    let payload = reply.get("payload").ok_or("missing signed reply payload")?;
    let bytes = aterm_json::to_vec(payload).map_err(|e| e.to_string())?;
    if !aterm_net::fleet::verifies_record(&key, RESULT_DOMAIN, &bytes, &decoded_signature(reply)?) {
        return Err("service repair response signature does not verify".into());
    }
    Ok(payload)
}
fn read_signed(path: &Path) -> Result<Value, String> {
    let bytes = aterm_secrets::fsx::read_owned(path, aterm_agent::service_repair_rpc::MAX_REQUEST)
        .map_err(|e| e.to_string())?
        .ok_or("signed activation request is missing")?;
    aterm_json::from_slice(&bytes).map_err(|e| e.to_string())
}
fn sign_prepared(
    prepared: &Value,
    identity: &aterm_net::fleet::NodeIdentity,
) -> Result<Value, String> {
    let request = transaction::parse_request(prepared).map_err(|e| e.to_string())?;
    if request.requester != identity.node() {
        return Err("remote prepared request names another requester".into());
    }
    let body = transaction::request_value(&request);
    let signature = identity.sign_record(
        REQUEST_DOMAIN,
        &aterm_json::to_vec(&body).map_err(|e| e.to_string())?,
    );
    Ok(Value::Object(aterm_json::Map::from([
        ("request".to_string(), body),
        ("key".to_string(), identity.key().hex().into()),
        (
            "signature".to_string(),
            aterm_codec::hex::encode_lower(&signature).into(),
        ),
    ])))
}

fn valid_job_word(job: &str) -> bool {
    matches!(job, "server" | "subscriber")
}

fn valid_unit_payload(unit: &Value) -> bool {
    unit.is_null()
        || (unit["program"].as_str().is_some()
            && unit["args"].is_array()
            && unit["definition"].as_str().is_some()
            && unit["image"].as_str().is_some()
            && unit["executable_sha256"].as_str().is_some())
}

fn result_matches(operation: &str, input: &Value, result: &Value) -> Result<(), String> {
    let outcome = result["schema"].as_u64() == Some(1)
        && matches!(
            result["verdict"].as_str(),
            Some("committed" | "rolled-back" | "refused" | "rollback-failed")
        );
    let expected_job = input["job"].as_str().and_then(|job| match job {
        "server" => Some(aterm_secrets::service::Job::Server.label()),
        "subscriber" => Some(aterm_secrets::service::Job::Subscriber.label()),
        _ => None,
    });
    let valid = match operation {
        "prepare" => {
            expected_job.is_some()
                && result["job"].as_str() == expected_job
                && result["desired_image"] == input["image"]
                && result["nonce"] == input["nonce"]
        }
        "activate" => outcome && result["request"] == input["signed"]["request"],
        "recover" => {
            expected_job.is_some()
                && ((result["state"].as_str() == Some("clean")
                    && result.get("job").is_none_or(|j| j.as_str() == expected_job))
                    || (outcome && result["request"]["job"].as_str() == expected_job))
        }
        "inspect" => {
            expected_job.is_some()
                && result.get("job").is_none_or(|j| j.as_str() == expected_job)
                && result["off"].as_bool().is_some()
                && result
                    .get("pin")
                    .is_some_and(|v| v.is_null() || v.as_str().is_some())
                && result.get("unit").is_some_and(valid_unit_payload)
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("service repair result does not match the requested operation".into())
    }
}

/// Execute `--host HOST inspect|prepare|activate|recover` without sending paths or argv.
/// Prepare writes its signed request locally; activate reads that same local request.
/// # Errors
/// Invalid options, unreadable local input, refused Drive, malformed signed response,
/// wrong prepared requester, local output failure, or unconfirmed remote completion.
pub(super) fn execute(args: &[String]) -> Result<Value, String> {
    let args = super::service_repair::strip_standalone_json(args);
    if args.len() < 3 || args[0] != "--host" || args[1].is_empty() {
        return Err("expected --host HOST inspect|prepare|activate|recover".into());
    }
    let host = &args[1];
    let operation = args[2].as_str();
    let rest = &args[3..];
    let mut local_identity = None;
    let payload = match operation {
        "inspect" | "recover" => {
            options(rest, &["--job"])?;
            let job = option(rest, "--job")?;
            if !valid_job_word(job) {
                return Err("job must be server or subscriber".into());
            }
            Value::Object(aterm_json::Map::from([("job".to_string(), job.into())]))
        }
        "prepare" => {
            options(rest, &["--job", "--image", "--nonce", "--out"])?;
            let job = option(rest, "--job")?;
            let image = option(rest, "--image")?;
            let nonce = option(rest, "--nonce")?;
            let out = option(rest, "--out")?;
            if !valid_job_word(job) {
                return Err("job must be server or subscriber".into());
            }
            if !super::service_repair::valid_image_sha256(image) {
                return Err("invalid service image sha256".into());
            }
            if !super::service_repair::valid_nonce(nonce) || out.is_empty() {
                return Err(
                    "nonce must be 16..128 ASCII letters, digits or hyphens and --out non-empty"
                        .into(),
                );
            }
            local_identity = Some(identity()?);
            Value::Object(aterm_json::Map::from([
                ("job".to_string(), job.into()),
                ("image".to_string(), image.into()),
                ("nonce".to_string(), nonce.into()),
            ]))
        }
        "activate" => {
            options(rest, &["--request"])?;
            let path = option(rest, "--request")?;
            if path.is_empty() {
                return Err("--request must be non-empty".into());
            }
            let signed = read_signed(Path::new(path))?;
            transaction::parse_request(&signed["request"]).map_err(|e| e.to_string())?;
            Value::Object(aterm_json::Map::from([("signed".to_string(), signed)]))
        }
        _ => {
            return Err(
                "remote service repair supports inspect, prepare, activate and recover".into(),
            );
        }
    };
    let reply = aterm_agent::service_repair_rpc::request(host, operation, &payload)?;
    let value = reply_payload(&reply)?;
    result_matches(operation, &payload, value)?;
    if operation == "prepare" {
        if value["node"].as_str() != reply["node"].as_str() {
            return Err("remote prepared request names another target".into());
        }
        let identity = match local_identity {
            Some(identity) => identity,
            None => identity()?,
        };
        let signed = sign_prepared(value, &identity)?;
        let bytes = aterm_json::to_vec(&signed).map_err(|e| e.to_string())?;
        aterm_secrets::fsx::write_atomic(Path::new(option(rest, "--out")?), &bytes, 0o600)
            .map_err(|e| e.to_string())?;
        return Ok(signed);
    }
    // Keep the authenticated remote wrapper intact in the CLI's JSON output.
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_json::json;
    #[test]
    fn remote_options_admit_only_explicit_paired_fields() {
        let args = vec!["--job".into(), "server".into()];
        assert!(options(&args, &["--job"]).is_ok());
        assert!(options(&["--job".into()], &["--job"]).is_err());
        assert!(
            options(
                &[
                    "--job".into(),
                    "server".into(),
                    "--job".into(),
                    "subscriber".into()
                ],
                &["--job"]
            )
            .is_err()
        );
        assert!(options(&["--program".into(), "/bin/sh".into()], &["--job"]).is_err());
    }
    #[test]
    fn tampered_result_signature_is_rejected() {
        let identity =
            aterm_net::fleet::NodeIdentity::from_seed("0123456789abcdef0123456789abcdef", &[7; 32])
                .unwrap();
        let payload = json!({"state":"clean"});
        let signature = identity.sign_record(RESULT_DOMAIN, &aterm_json::to_vec(&payload).unwrap());
        let mut reply = json!({"payload":payload,"node":identity.node(),"key":identity.key().hex(),"signature":aterm_codec::hex::encode_lower(&signature)});
        assert!(reply_payload(&reply).is_ok());
        reply
            .as_object_mut()
            .unwrap()
            .insert("payload".into(), json!({"state":"committed"}));
        assert!(reply_payload(&reply).is_err());
    }

    #[test]
    fn authentic_results_still_must_match_the_requested_operation() {
        let input = json!({"job":"server","image":"new-image","nonce":"requested-nonce"});
        let result = json!({"job":aterm_secrets::service::Job::Server.label(),"desired_image":"new-image","nonce":"requested-nonce"});
        assert!(result_matches("prepare", &input, &result).is_ok());
        for field in ["job", "image", "nonce"] {
            let mut other = input.clone();
            other
                .as_object_mut()
                .unwrap()
                .insert(field.into(), json!("different"));
            assert!(result_matches("prepare", &other, &result).is_err());
        }
        let activate = json!({"signed":{"request":result.clone()}});
        assert!(
            result_matches(
                "activate",
                &activate,
                &json!({"schema":1,"verdict":"committed","request":result})
            )
            .is_ok()
        );
        assert!(
            result_matches("activate", &activate, &json!({"request":{"nonce":"other"}})).is_err()
        );
        assert!(result_matches("recover", &input, &json!({"state":"clean"})).is_ok());
        assert!(
            result_matches(
                "recover",
                &json!({"job":"bogus"}),
                &json!({"state":"clean"})
            )
            .is_err()
        );
        assert!(
            result_matches(
                "recover",
                &input,
                &json!({"state":"clean","job":aterm_secrets::service::Job::Subscriber.label()})
            )
            .is_err()
        );
        assert!(
            result_matches(
                "recover",
                &input,
                &json!({"request":{"job":aterm_secrets::service::Job::Subscriber.label()}})
            )
            .is_err()
        );
        assert!(
            result_matches(
                "inspect",
                &input,
                &json!({"job":aterm_secrets::service::Job::Server.label(),"unit":null,"off":false,"pin":null})
            )
            .is_ok()
        );
        assert!(result_matches("inspect", &input, &json!({"state":"clean"})).is_err());
        assert!(
            result_matches(
                "inspect",
                &input,
                &json!({"job":aterm_secrets::service::Job::Subscriber.label(),"unit":null,"off":false,"pin":null})
            )
            .is_err()
        );
        // Invalid local options refuse before dialing any remote host.
        assert!(
            execute(&[
                "--host".into(),
                "unreachable.invalid".into(),
                "inspect".into(),
                "--job".into(),
                "bogus".into(),
            ])
            .is_err()
        );
        assert!(
            execute(&[
                "--host".into(),
                "unreachable.invalid".into(),
                "prepare".into(),
                "--job".into(),
                "server".into(),
                "--image".into(),
                "not-hex".into(),
                "--nonce".into(),
                "n".into(),
                "--out".into(),
                "/tmp/out".into(),
            ])
            .is_err()
        );
        assert!(
            execute(&[
                "--host".into(),
                "unreachable.invalid".into(),
                "prepare".into(),
                "--job".into(),
                "server".into(),
                "--image".into(),
                "a".repeat(64),
                "--nonce".into(),
                "n".into(),
                "--out".into(),
                String::new(),
            ])
            .is_err()
        );
        // Standalone `--json` does not turn valid remote flags into an option-count error, while
        // invalid `--job` still fails locally before dialing.
        let json_err = execute(&[
            "--json".into(),
            "--host".into(),
            "unreachable.invalid".into(),
            "inspect".into(),
            "--job".into(),
            "bogus".into(),
            "--json".into(),
        ])
        .unwrap_err();
        assert_eq!(json_err, "job must be server or subscriber");
    }
}
