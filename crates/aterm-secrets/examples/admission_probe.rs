// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inspect one host's historical admission in the locally applied admin snapshot.
//! This reads no server, changes no state and prints no credential values.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use aterm_secrets::client::{self, AdmissionMethod, Paths};

    let mut args = std::env::args_os().skip(1);
    let host = args
        .next()
        .ok_or("usage: admission_probe <host>")?
        .into_string()
        .map_err(|_| "host must be valid UTF-8")?;
    if host.is_empty()
        || host.len() > 128
        || host.chars().any(char::is_control)
        || args.next().is_some()
    {
        return Err("usage: admission_probe <host>".into());
    }
    let paths = Paths::live()?;
    let admin = client::admin_cached(&paths)?;
    let admission = admin.commons_admission(&host);
    let recipient = admin
        .doc
        .machines()
        .into_iter()
        .find(|machine| machine.name == host)
        .map(|machine| machine.recipient);
    let method = admission.map(|evidence| match evidence.method {
        AdmissionMethod::Founder => "founder",
        AdmissionMethod::Password => "password",
        AdmissionMethod::Manual => "manual",
        AdmissionMethod::Open => "open",
    });
    println!(
        "{}",
        aterm_json::json!({
            "schema": 1,
            "kind": "vault-admission-evidence",
            "scope": "locally-applied-admin-snapshot",
            "vault": admin.config.name,
            "snapshot_sequence": admin.seq(),
            "host": host,
            "recipient": recipient,
            "recorded": admission.is_some(),
            "method": method,
            "decision_sequence": admission.map(|evidence| evidence.sequence),
            "authored_ms": admission.map(|evidence| evidence.authored_ms),
            "author": admission.map(|evidence| evidence.author.to_bech32()),
            "grants_membership": false,
        })
    );
    Ok(())
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("admission_probe requires Unix vault support");
    std::process::ExitCode::from(2)
}
