// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn configured_credentials_and_overbound_records_never_reach_diagnostics() {
    let secret = concat!("opaque-private-", "build-credential-", "123456789");
    let mut run = Run::new("fixture", ["build"]);
    run.clear_env = true;
    run.env
        .push(("FIXTURE_PRIVATE_TOKEN".into(), secret.into()));
    let message = aterm_json::json!({
        "reason": "compiler-message",
        "message": {"level": "error", "rendered": format!("error: {secret}")}
    });
    let mut output = Ran {
        code: Some(101),
        stdout: message.to_string().into_bytes(),
        stderr: format!("stderr echoed {secret}").into_bytes(),
    };
    let failure = Failure::capture(&run, &output);
    assert!(!failure.compiler.contains(secret));
    assert!(!failure.stderr.contains(secret));
    assert!(failure.compiler.contains("[redacted]"));

    output.stdout = vec![b'x'; INPUT_BYTES + 1];
    let failure = Failure::capture(&run, &output);
    assert!(failure.compiler.contains("input/count bound"));
    assert!(failure.compiler.len() <= ERROR_BYTES);

    output.stdout = (0..ERROR_COUNT + 3)
        .map(|index| {
            aterm_json::json!({
                "reason": "compiler-message",
                "message": {"level": "error", "rendered": format!("error: fixture-{index}")}
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes();
    let failure = Failure::capture(&run, &output);
    assert_eq!(
        failure.compiler.matches("error: fixture-").count(),
        ERROR_COUNT
    );
    assert!(failure.compiler.contains("input/count bound"));
}
