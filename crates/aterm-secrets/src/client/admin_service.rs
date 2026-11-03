// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A narrow admission operation for an already authenticated owner or fleet driver.
//! It cannot edit secrets, restore held identities, or run a command. Its receipt
//! proves publication observed at the source, not delivery to the joining machine.

use super::*;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

const DOMAIN: &[u8] = b"aterm/vault/admission-receipt/1\0";

/// One explicitly named queued join and a caller-chosen correlation nonce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The code shown by the joining machine.
    pub code: String,
    /// A 128-bit lowercase hexadecimal correlation value.
    pub nonce: String,
}

impl Request {
    /// Parse the bounded control command operands.
    ///
    /// # Errors
    /// The operation, code, nonce or argument count is invalid.
    pub fn parse(text: &str) -> crate::Result<Self> {
        let words: Vec<_> = text.split_whitespace().collect();
        let ["approve", code, nonce] = words.as_slice() else {
            return Err(Error::new(
                "vault approve <join-code> <32 lowercase hex digits>",
            ));
        };
        if code.len() != 19
            || !code
                .split('-')
                .all(|part| part.len() == 4 && part.bytes().all(|c| c.is_ascii_alphanumeric()))
            || nonce.len() != 32
            || !nonce
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::new("invalid admission code or nonce"));
        }
        Ok(Self {
            code: code.to_ascii_lowercase(),
            nonce: (*nonce).to_owned(),
        })
    }
}

/// Verify a receipt against a separately trusted vault signing key. The returned
/// JSON binds the caller, nonce, exact recipient, vault, sequence and host.
///
/// # Errors
/// The JSON, bounded payload, pinned signer or signature is invalid.
pub fn verify_receipt(text: &str, signer: &[u8; 32]) -> crate::Result<aterm_json::Value> {
    if text.len() > 8192 {
        return Err(Error::new("admission receipt exceeds its bound"));
    }
    let value: aterm_json::Value =
        aterm_json::from_str(text).map_err(|_| Error::new("invalid admission receipt"))?;
    let payload = value["payload"]
        .as_str()
        .ok_or_else(|| Error::new("receipt has no payload"))?;
    if value["signer"]
        .as_str()
        .and_then(crate::b64::decode_padded)
        .as_deref()
        != Some(signer.as_slice())
    {
        return Err(Error::new(
            "admission receipt signer differs from the trusted key",
        ));
    }
    let signature = value["signature"]
        .as_str()
        .and_then(crate::b64::decode_padded)
        .ok_or_else(|| Error::new("receipt has no signature"))?;
    UnparsedPublicKey::new(&ED25519, signer)
        .verify(&[DOMAIN, payload.as_bytes()].concat(), &signature)
        .map_err(|_| Error::new("admission receipt signature failed"))?;
    aterm_json::from_str(payload).map_err(|_| Error::new("invalid signed admission payload"))
}

/// Verify the signature and its binding to this caller's exact request, including
/// the code derived from the signed recipient and a valid host and sequence.
///
/// # Errors
/// The signature, expected request, recipient, host, sequence or outcome differs.
pub fn verify_for_request(
    text: &str,
    signer: &[u8; 32],
    vault: &str,
    caller: &str,
    request: &Request,
) -> crate::Result<aterm_json::Value> {
    let value = verify_receipt(text, signer)?;
    let recipient = value["recipient"]
        .as_str()
        .and_then(|key| Recipient::parse(key).ok())
        .ok_or_else(|| Error::new("receipt has no valid recipient"))?;
    if value["schema"].as_u64() != Some(1)
        || value["kind"].as_str() != Some("vault-admission")
        || value["outcome"].as_str() != Some("publication-observed")
        || value["caller"].as_str() != Some(caller)
        || value["vault"].as_str() != Some(vault)
        || value["nonce"].as_str() != Some(request.nonce.as_str())
        || value["code"].as_str() != Some(request.code.as_str())
        || join_code(&recipient) != request.code
        || !value["host"].as_str().is_some_and(text::valid_machine_name)
        || !value["vault"].as_str().is_some_and(envelope::valid_name)
        || !value["sequence"].as_u64().is_some_and(|seq| seq > 0)
        || !value["head"].as_str().is_some_and(|head| {
            head.len() == 64
                && head
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
    {
        return Err(Error::new(
            "admission receipt does not bind the expected request",
        ));
    }
    Ok(value)
}

fn receipt(
    admin: &Admin,
    caller: &str,
    request: &Request,
    recipient: &str,
    host: &str,
) -> crate::Result<String> {
    let head = admin
        .opened_heads
        .first()
        .ok_or_else(|| Error::new("admission has no authenticated source head"))?;
    let payload = aterm_json::to_string(&aterm_json::json!({
        "schema": 1, "kind": "vault-admission", "caller": caller,
        "nonce": request.nonce, "code": request.code, "recipient": recipient,
        "host": host, "vault": admin.config.name, "sequence": admin.seq,
        "head": aterm_codec::hex::encode_lower(head),
        "outcome": "publication-observed",
    }))
    .map_err(|_| Error::new("cannot encode admission payload"))?;
    let key = Ed25519KeyPair::from_seed_unchecked(&admin.seed)
        .map_err(|_| Error::new("invalid vault signing key"))?;
    let signature = key.sign(&[DOMAIN, payload.as_bytes()].concat());
    aterm_json::to_string(&aterm_json::json!({
        "payload": payload, "signer": crate::b64::encode_padded(key.public_key().as_ref()),
        "signature": crate::b64::encode_padded(signature.as_ref()),
    }))
    .map_err(|_| Error::new("cannot encode admission receipt"))
}

/// Admit one code using the existing administrator publication and read-back
/// transaction. A replay of an admitted code only reads its published state.
/// Authentication and live fleet drive authorization belong to the transport.
///
/// # Errors
/// Administrative custody is unavailable, the code is ambiguous or unknown,
/// the identity is held/removed, or publication or authenticated read-back fails.
pub fn approve(paths: &Paths, caller: &str, request: &Request) -> crate::Result<String> {
    let checked = Request::parse(&format!("approve {} {}", request.code, request.nonce))?;
    let request = &checked;
    if caller.is_empty() || caller.len() > 128 || caller.chars().any(char::is_control) {
        return Err(Error::new("invalid admission caller"));
    }
    let admin = admin_open(paths)?;
    if member_list(paths)?.is_some_and(|members| {
        members
            .iter()
            .any(|m| m.code.eq_ignore_ascii_case(&request.code) && m.removed.is_some())
    }) {
        return Err(Error::new(
            "a removed identity needs its existing restoration procedure",
        ));
    }
    let existing: Vec<_> = admin
        .doc
        .machines()
        .into_iter()
        .filter(|m| {
            Recipient::parse(&m.recipient)
                .is_ok_and(|r| join_code(&r).eq_ignore_ascii_case(&request.code))
        })
        .collect();
    let recipient = match existing.as_slice() {
        [machine] => machine.recipient.clone(),
        [] => {
            let observed = requests_for_approval(&admin, &[ApprovalTarget::Code(&request.code)])?;
            let pending = &observed.requests;
            let matches: Vec<_> = pending
                .iter()
                .filter(|r| r.code.eq_ignore_ascii_case(&request.code))
                .collect();
            let Some(request) = matches.first() else {
                return Err(Error::new("admission code is unknown or ambiguous"));
            };
            one_name_asked(pending, request)?;
            if admin.doc.code_only().contains(&request.recipient) {
                return Err(Error::new(
                    "a held identity needs local administrative approval",
                ));
            }
            let recipient = request.recipient.clone();
            // Keep the exact administrative baseline whose holds were checked:
            // reopening here could silently turn a concurrent hold into an approval.
            approve_observed_request_opened(paths, &admin, &observed, &recipient)?;
            recipient
        }
        _ => return Err(Error::new("admission code is ambiguous")),
    };
    let current = admin_open(paths)?;
    if current.doc.code_only().contains(&recipient) {
        return Err(Error::new(
            "a held identity needs local administrative approval",
        ));
    }
    verify_approved_in_served_head(
        paths,
        &current,
        current.seq,
        std::slice::from_ref(&recipient),
    )?;
    let current_host = current
        .doc
        .machines()
        .into_iter()
        .find(|m| m.recipient == recipient)
        .map(|m| m.name)
        .ok_or_else(|| Error::new("the approved recipient is no longer in the vault"))?;
    receipt(&current, caller, request, &recipient, &current_host)
}

/// Serve a fixed operation after control admission, without a shell or GUI action.
pub fn serve(caller: &str, rest: &str) -> String {
    if rest.trim() == "status" {
        return serve_result(Paths::live().and_then(|paths| status(&paths)));
    }
    serve_result(
        Request::parse(rest).and_then(|request| approve(&Paths::live()?, caller, &request)),
    )
}

/// Serve using the node's configured vault, with no fallback to the owner's home
/// when an isolated node deliberately has no vault.
pub fn serve_at(paths: Option<&Paths>, caller: &str, rest: &str) -> String {
    if rest.trim() == "status" {
        return serve_result(
            paths
                .ok_or_else(|| Error::new("this node follows no vault"))
                .and_then(status),
        );
    }
    serve_result(Request::parse(rest).and_then(|request| {
        approve(
            paths.ok_or_else(|| Error::new("this node follows no vault"))?,
            caller,
            &request,
        )
    }))
}

fn status(paths: &Paths) -> crate::Result<String> {
    let config = config(paths)?;
    let capability = match can_administer(paths) {
        Ok(value) => aterm_json::json!({"state":"known","value":value}),
        Err(error) => aterm_json::json!({"state":"unknown","reason":error.to_string()}),
    };
    aterm_json::to_string(&aterm_json::json!({
        "schema":1, "kind":"vault-admin-capability",
        "vault":config.as_ref().map(|c| &c.name),
        "signer":config.as_ref().and_then(|c| c.signer).map(|s| crate::b64::encode_padded(&s)),
        "vault_admin":capability,
    }))
    .map_err(|_| Error::new("cannot encode vault capability"))
}

fn serve_result(result: crate::Result<String>) -> String {
    match result {
        Ok(receipt) => format!("OK 1\n{receipt}\n"),
        Err(error) => format!(
            "ERR vault: {}\n",
            error.to_string().replace(['\r', '\n'], " ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_publishes_one_recipient_replays_and_respects_a_hold() {
        let _answering = ServersAnswering::set();
        let root = aterm_tempfile::tempdir().unwrap();
        let server = crate::server::start_with_join_change_gap(
            std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
            Duration::ZERO,
        )
        .unwrap();
        let url = format!("http://{}/v1/team", server.addr);
        let paths = Paths::for_home(&root.path().join("admin"));
        create(&paths, &url, None, Some("admin")).unwrap();
        let reported: aterm_json::Value = aterm_json::from_str(&status(&paths).unwrap()).unwrap();
        assert_eq!(reported["vault_admin"]["value"].as_bool(), Some(true));
        let queued = |host: &str| {
            let identity = machine_identity(&Paths::for_home(&root.path().join(host))).unwrap();
            let recipient = identity.recipient();
            let code = join_code(&recipient);
            transport::post_join(&url, &format!("{} {host} {code}", recipient.to_bech32()))
                .unwrap();
            (
                recipient.to_bech32(),
                Request {
                    code,
                    nonce: "0123456789abcdef0123456789abcdef".into(),
                },
            )
        };
        let (first, request) = queued("first");
        let (held, held_request) = queued("held");
        let (raced, _) = queued("raced");
        let admin = admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_code_only(&held).unwrap();
        doc.add_code_only(&raced).unwrap();
        admin_publish(&paths, &admin, &doc).unwrap();
        let pin = admin.config.signer.unwrap();
        assert!(
            approve_request_opened(&paths, &admin, &raced).is_err(),
            "a hold published after approval's baseline must win the source comparison"
        );
        assert!(approve(&paths, "fleet:test", &held_request).is_err());
        let wire = approve(&paths, "fleet:test", &request).unwrap();
        let receipt = verify_for_request(&wire, &pin, "team", "fleet:test", &request).unwrap();
        assert_eq!(receipt["recipient"].as_str(), Some(first.as_str()));
        assert!(verify_for_request(&wire, &pin, "team", "fleet:other", &request).is_err());
        assert!(verify_for_request(&wire, &pin, "another-vault", "fleet:test", &request).is_err());
        let mut changed = request.clone();
        changed.nonce = "abcdef0123456789abcdef0123456789".into();
        assert!(verify_for_request(&wire, &pin, "team", "fleet:test", &changed).is_err());
        changed = held_request.clone();
        assert!(verify_for_request(&wire, &pin, "team", "fleet:test", &changed).is_err());
        let before = admin_open(&paths).unwrap().seq;
        assert!(approve(&paths, "fleet:test", &request).is_ok());
        assert_eq!(
            admin_open(&paths).unwrap().seq,
            before,
            "a replay cannot republish"
        );
        assert!(
            !admin_open(&paths)
                .unwrap()
                .doc
                .machines()
                .iter()
                .any(|m| m.recipient == held)
        );
        let stranger = Paths::for_home(&root.path().join("stranger"));
        let absent: aterm_json::Value = aterm_json::from_str(&status(&stranger).unwrap()).unwrap();
        assert_eq!(absent["vault_admin"]["value"].as_bool(), Some(false));
        let held_lock = StateLock::take(&paths).unwrap();
        let busy: aterm_json::Value = aterm_json::from_str(&status(&paths).unwrap()).unwrap();
        assert_eq!(busy["vault_admin"]["state"].as_str(), Some("unknown"));
        assert!(busy["vault_admin"].get("value").is_none());
        drop(held_lock);
        assert!(approve(&stranger, "fleet:test", &request).is_err());
        server.stop();
        assert!(
            approve(&paths, "fleet:test", &request).is_err(),
            "a local cached success is not served-head proof"
        );
    }

    #[test]
    fn admission_accepts_only_one_code_and_a_bounded_nonce() {
        let good = "approve abcd-efgh-ijkl-mnop 0123456789abcdef0123456789abcdef";
        assert!(Request::parse(good).is_ok());
        for bad in [
            "approve --all",
            "approve abcd-efgh-ijkl-mnop x",
            "edit",
            "approve abcd-efgh-ijkl-mnop 0123456789abcdef0123456789abcdef command=sh",
        ] {
            assert!(Request::parse(bad).is_err());
        }
    }

    #[test]
    fn signed_receipt_rejects_changed_caller_recipient_nonce_and_trust_root() {
        let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let signer: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let payload = r#"{"caller":"fleet:m1","recipient":"age1receiver","nonce":"0123456789abcdef0123456789abcdef","host":"joining","vault":"team","sequence":5}"#;
        let signature = key.sign(&[DOMAIN, payload.as_bytes()].concat());
        let encode = |payload: &str| {
            aterm_json::to_string(&aterm_json::json!({
                "payload": payload, "signer": crate::b64::encode_padded(&signer),
                "signature": crate::b64::encode_padded(signature.as_ref()),
            }))
            .unwrap()
        };
        let wire = encode(payload);
        assert_eq!(
            verify_receipt(&wire, &signer).unwrap()["caller"].as_str(),
            Some("fleet:m1")
        );
        for changed in [
            payload.replace("m1", "m2"),
            payload.replace("receiver", "attacker"),
            payload.replace("012345", "abcdef"),
            payload.replace("joining", "other"),
            payload.replace("team", "other-vault"),
            payload.replace(":5", ":6"),
        ] {
            assert!(verify_receipt(&encode(&changed), &signer).is_err());
        }
        assert!(verify_receipt(&wire, &[8; 32]).is_err());
        let wrong_domain = key.sign(payload.as_bytes());
        let forged = aterm_json::to_string(&aterm_json::json!({"payload":payload,"signer":crate::b64::encode_padded(&signer),"signature":crate::b64::encode_padded(wrong_domain.as_ref())})).unwrap();
        assert!(verify_receipt(&forged, &signer).is_err());
    }
}
