// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::SecretBytes;

#[test]
fn deployer_contributions_require_the_consumers_rsa_pkcs8_shape_without_echoing_bad_input() {
    let der =
        include_bytes!("../../../atpkg/src/vendor_direct/fixtures/openpgp-test-rsa4096.pk8.der");
    let pem = format!(
        "{}\n{}\n{}\n",
        concat!("-----BEGIN ", "PRIVATE KEY-----"),
        aterm_codec::base64::encode(der).unwrap(),
        concat!("-----END ", "PRIVATE KEY-----")
    );
    let mut json = aterm_json::json!({
        "type":"service_account", "client_email":"fixture@example.invalid",
        "token_uri":"https://example.invalid/token", "private_key":pem,
    });
    let values = |json: &Value| {
        Values(vec![(
            Want::SiteDeployerKey.entries()[0].clone(),
            SecretBytes::from(json.to_string()),
        )])
    };
    assert_eq!(
        validate(&Want::SiteDeployerKey, &values(&json))
            .unwrap()
            .as_deref(),
        Some("fixture@example.invalid")
    );
    json.as_object_mut().unwrap().insert(
        "private_key".into(),
        Value::from("bad-key-sentinel-never-print"),
    );
    let error = validate(&Want::SiteDeployerKey, &values(&json))
        .unwrap_err()
        .to_string();
    assert!(!error.contains("bad-key-sentinel"));
    json.as_object_mut()
        .unwrap()
        .insert("type".into(), Value::from("authorized_user"));
    assert!(validate(&Want::SiteDeployerKey, &values(&json)).is_err());
}

#[test]
fn an_azure_principal_contribution_is_the_three_variables_signtool_takes() {
    let values = |text: &str| {
        Values(vec![(
            Want::AzureSigningPrincipal.entries()[0].clone(),
            SecretBytes::from(text.to_owned()),
        )])
    };
    let whole = "AZURE_TENANT_ID=t\nAZURE_CLIENT_ID=c\nAZURE_CLIENT_SECRET=s\n";
    assert_eq!(
        validate(&Want::AzureSigningPrincipal, &values(whole)).unwrap(),
        None
    );
    let error = validate(
        &Want::AzureSigningPrincipal,
        &values("AZURE_TENANT_ID=t\nAZURE_CLIENT_SECRET=sentinel-never-print\n"),
    )
    .unwrap_err()
    .to_string();
    assert!(!error.contains("sentinel"), "{error}");
}

#[test]
fn machines_admin_contributions_are_one_line() {
    let values = |bytes: &[u8]| {
        Values(vec![(
            Want::MachinesAdmin.entries()[0].clone(),
            SecretBytes::from(bytes.to_vec()),
        )])
    };
    assert_eq!(
        validate(&Want::MachinesAdmin, &values(b"pa ss-word\n")).unwrap(),
        None
    );
    assert!(validate(&Want::MachinesAdmin, &values(b"one\ntwo\n")).is_err());
    assert!(validate(&Want::MachinesAdmin, &values(b"\n")).is_err());
}

/// A paper master contribution is one line of 52 phrase symbols; a refusal holds none of them.
#[test]
fn paper_master_contributions_are_one_phrase_and_never_echoed() {
    const PHRASE: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";
    let values = |text: &str| {
        Values(vec![(
            Want::PaperMaster.entries()[0].clone(),
            SecretBytes::from(text.to_owned()),
        )])
    };
    assert_eq!(validate(&Want::PaperMaster, &values(PHRASE)).unwrap(), None);
    assert_eq!(
        validate(&Want::PaperMaster, &values(&format!("{PHRASE}\n"))).unwrap(),
        None
    );
    for bad in [
        &PHRASE[1..],
        &PHRASE.replacen('5', "u", 1),
        &format!("{}\n{}", &PHRASE[..26], &PHRASE[26..]),
    ] {
        let error = validate(&Want::PaperMaster, &values(bad))
            .unwrap_err()
            .to_string();
        assert!(!error.contains(&PHRASE[..8]), "{error}");
    }
}
