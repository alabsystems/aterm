// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An inspector asks only about its authenticated peer tree. It supplies no PID,
//! resource ceiling, claim pathname or environment assertion as authority.
use std::{
    ffi::OsString,
    io::{self, Write as _},
    process::ExitCode,
};

pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    let (value, code) = match inspect(argv) {
        Ok(value) => {
            let code = if value["ready"].as_bool() == Some(true) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(75)
            };
            (value, code)
        }
        Err(_) => (
            aterm_types::resource_inspection::unavailable("authenticated host custody unavailable"),
            ExitCode::from(75),
        ),
    };
    if writeln!(crate::StdoutSink::new(io::stdout().lock()), "{value}").is_err() {
        return ExitCode::from(75);
    }
    code
}
fn inspect(argv: Vec<OsString>) -> io::Result<aterm_json::Value> {
    let mut args = argv.into_iter();
    let mut session = None;
    let mut json = false;
    while let Some(word) = args.next() {
        let slot = match word.to_str() {
            Some("--json") if !json => {
                json = true;
                continue;
            }
            Some("--session") if session.is_none() => &mut session,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "usage: aterm resource-inspect --json [--session @self|@<sid>]",
                ));
            }
        };
        *slot = Some(
            args.next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| io::Error::other("missing inspector option"))?,
        );
    }
    if !json {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource-inspect requires --json",
        ));
    }
    let session = session.unwrap_or_else(|| "@self".into());
    let self_sid = std::env::var(crate::SELF_SID_ENV)
        .ok()
        .filter(|sid| !sid.is_empty());
    // SID is a selector only. Resolve it locally so a closed/restarted GUI
    // cannot prevent the exact cached helper querying its actual custodian.
    let sid = if session == "@self" {
        self_sid.ok_or_else(|| io::Error::other("native session selector unavailable"))?
    } else if let Some(sid) = session.strip_prefix('@') {
        sid.to_owned()
    } else {
        return Err(io::Error::other(
            "native inspector requires @self or @<sid>",
        ));
    };
    crate::native_resource_inspection::lookup(&sid)
}
