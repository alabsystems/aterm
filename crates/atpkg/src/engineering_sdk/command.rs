// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit native producer entry. No configured prefix, environment defaults, or activation.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::ExitCode;
use std::time::{Duration, Instant};

pub(crate) const USAGE: &str = "atpkg engineering-sdk --base-prefix PATH --base-build N --targo IMAGE --targo-sha256 HEX --expected-targo-source COMMIT --destination FRESH_PRIVATE_PREFIX --build N --timeout-seconds N\nConstructs an unverified local engineering artifact; no current, pin, package or release selection.";
const FLAGS: &[&str] = &[
    "--base-prefix",
    "--base-build",
    "--targo",
    "--targo-sha256",
    "--expected-targo-source",
    "--destination",
    "--build",
    "--timeout-seconds",
];

fn parse(args: &[OsString]) -> Result<(super::Request, Duration), String> {
    let mut fields = BTreeMap::new();
    let (pairs, remainder) = args.as_chunks::<2>();
    for pair in pairs {
        let flag = pair[0].to_str().ok_or("non-UTF8 option")?;
        if !FLAGS.contains(&flag) || fields.insert(flag, pair[1].clone()).is_some() {
            return Err(format!("unknown or duplicate engineering option {flag}"));
        }
    }
    if !remainder.is_empty() || fields.len() != FLAGS.len() {
        return Err("all engineering options require exactly one explicit value".into());
    }
    let text = |name| {
        fields[name]
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("non-UTF8 {name}"))
    };
    let number = |name| -> Result<u64, String> {
        let value = text(name)?;
        if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(format!("{name} must be a canonical positive integer"));
        }
        value.parse().map_err(|_| format!("{name} exceeds u64"))
    };
    let timeout = number("--timeout-seconds")?;
    if timeout > 3600 {
        return Err("engineering deadline exceeds one hour".into());
    }
    Ok((
        super::Request {
            base: crate::store::Layout {
                prefix: fields["--base-prefix"].clone().into(),
            },
            base_build: number("--base-build")?,
            targo: fields["--targo"].clone().into(),
            targo_sha256: text("--targo-sha256")?,
            expected_targo_source: text("--expected-targo-source")?,
            destination: fields["--destination"].clone().into(),
            build: number("--build")?,
        },
        Duration::from_secs(timeout),
    ))
}
pub(crate) fn command(args: &[OsString]) -> ExitCode {
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = parse(args)
        .map_err(super::invalid)
        .and_then(|(request, timeout)| {
            let deadline = Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| super::invalid("deadline overflow"))?;
            let candidate = super::produce(request, deadline)?;
            candidate.check_current(deadline)?;
            // Observations only; caller must separately qualify the unchanged Host/Keeper path.
            println!(
                "engineering-sdk: constructed {} (private prefix {}; no activation or proof claim)",
                candidate.directory().display(),
                candidate.layout().prefix.display()
            );
            Ok(())
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("engineering-sdk: {why}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
