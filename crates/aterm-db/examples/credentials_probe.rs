// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development entry for credential requests and value-free vault observations.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use aterm_json::json;
    use aterm_secrets::client::{self, Membership, Paths};
    use std::{ffi::OsStr, path::Path, process::ExitCode};

    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let [verb, words @ ..] = args.as_slice()
        && verb == OsStr::new("need")
    {
        return aterm_db::credentials::cli::main_entry(words.to_vec());
    }
    let [verb, home] = args.as_slice() else {
        eprintln!("usage: credentials_probe status <absolute-home> | need <words...>");
        return ExitCode::from(2);
    };
    let home = Path::new(home);
    if verb != OsStr::new("status") || !home.is_absolute() {
        eprintln!("usage: credentials_probe status <absolute-home> | need <words...>");
        return ExitCode::from(2);
    }
    let status = match client::status(&Paths::for_home(home)) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("credentials_probe: {error}");
            return ExitCode::FAILURE;
        }
    };
    let (membership, seq, heard) = match status.membership {
        Membership::Unconfigured => ("unconfigured", None, None),
        Membership::Pending => ("pending", None, None),
        Membership::In { seq, heard } => ("in", Some(seq), Some(heard)),
        Membership::Revoked { seq } => ("revoked", Some(seq), None),
        Membership::Plain => ("plain", None, None),
    };
    let value = json!({
        "membership":membership,"seq":seq,"heard":heard,
        "configured":status.config.is_some(),
        "alab":status.config.as_ref().map(client::alab_vault),
        "host":status.config.as_ref().map(|config|&config.host),
        "has_identity":status.machine.is_some(),
        "materialized_names":status.names.len(),
    });
    aterm_uds::stdout::answer_exit("credentials_probe", &format!("{value}\n"), 0)
}

#[cfg(not(unix))]
fn main() {}
