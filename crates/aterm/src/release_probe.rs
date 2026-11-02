// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Composition of the explicit probe with ordinary configured Git and existing fleet identity.

use aterm_secrets::Error;
use aterm_secrets::client::release::probe::{Node, Request};
use std::process::ExitCode;
use std::sync::Arc;

pub(super) fn run(args: &[std::ffi::OsString]) -> ExitCode {
    if args.first().is_none_or(|arg| arg != "probe") {
        eprintln!(
            "aterm release: expected probe run|status|resume|cleanup --nonce HEX --evidence-dir PATH [--repo OWNER/aterm] --json"
        );
        return ExitCode::from(2);
    }
    let args: Vec<String> = match args[1..]
        .iter()
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect()
    {
        Some(args) => args,
        None => {
            eprintln!("aterm release: arguments must be UTF-8");
            return ExitCode::from(2);
        }
    };
    let outcome = (|| {
        let request = Request::parse(&args)?;
        let paths = aterm_secrets::client::Paths::live()?;
        let state = aterm_types::dirs::state_dir()
            .ok_or_else(|| Error::new("probe has no current state directory"))?;
        let node = Arc::new(move || {
            let node = aterm_net::fleet_member::read_node_identity(&state)?
                .ok_or_else(|| Error::new("probe requires an existing fleet identity"))?;
            Ok(Node {
                id: node.node().to_owned(),
                key: *node.key().bytes(),
            })
        });
        let port = Arc::new(aterm_machines::release_probe::Ordinary::new(node));
        aterm_secrets::client::release::probe::execute(&paths, &request, port)
    })();
    match outcome {
        Ok(report) => {
            let complete = matches!(
                report.get("state").and_then(aterm_json::Value::as_str),
                Some("complete" | "clean" | "historical_unrevalidated")
            );
            match aterm_json::to_string(&report) {
                Ok(report) => aterm_uds::stdout::answer_exit(
                    "aterm release probe",
                    &format!("{report}\n"),
                    if complete { 0 } else { 1 },
                ),
                Err(_) => ExitCode::from(2),
            }
        }
        Err(error) => {
            eprintln!("aterm release probe: {error}");
            ExitCode::from(1)
        }
    }
}
