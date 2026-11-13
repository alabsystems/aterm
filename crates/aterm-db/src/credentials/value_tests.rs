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
