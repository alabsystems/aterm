// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit trusted launch control for one broker-aware request. Request files
//! and provider bodies never enter the ordinary CLI's unprotected JSON tree.

use super::{
    Broker, google,
    transport::{Reply, Status},
};
use crate::{Error, Result, database::decode_hex};
use aterm_codec::hex;
use aterm_json::json;
use aterm_secrets::{
    client::{self, Paths},
    fsx,
};
use aterm_uds::secret::Secret;
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

mod proposal;
#[cfg(test)]
pub(crate) use proposal::propose_at;
pub(crate) use proposal::run as policy_command;
#[cfg(test)]
pub(crate) use proposal::test_command_with_paths;

struct Request<'a> {
    selection: super::Selection,
    logical: [u8; 16],
    file: &'a str,
    within: Duration,
}

pub(crate) fn main_entry(words: &[String]) -> ExitCode {
    match run(words) {
        Ok(answer) => {
            let code = aterm_uds::stdout::answer_status(
                "aterm db accounts-request",
                aterm_uds::stdout::write_all(answer.expose()),
                0,
            )
            .unwrap_or_else(|line| {
                let _ = writeln!(std::io::stderr(), "{line}");
                1
            });
            ExitCode::from(code)
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "aterm db accounts-request: {error}");
            ExitCode::from(1)
        }
    }
}

fn parse(words: &[String]) -> Result<Request<'_>> {
    let (pairs, rest) = words.as_chunks::<2>();
    let mut options = BTreeMap::new();
    for pair in pairs {
        if options.insert(pair[0].as_str(), pair[1].as_str()).is_some() {
            return Err(Error::new("repeated account request option"));
        }
    }
    if !rest.is_empty() {
        return Err(Error::new("account request option needs a value"));
    }
    let selection = match (options.remove("--grant"), options.remove("--account")) {
        (Some(id), None) => {
            let id = decode_hex(id)?;
            if id == [0; 16] {
                return Err(Error::new("zero grant ID"));
            }
            super::Selection::Grant(id)
        }
        (None, Some(account)) => {
            let account = decode_hex(account)?;
            if account == [0; 32] {
                return Err(Error::new("zero account ID"));
            }
            super::Selection::Account {
                account,
                adapter: google::ADAPTER.into(),
            }
        }
        _ => return Err(Error::new("supply exactly one of --grant and --account")),
    };
    let logical = decode_hex(
        options
            .remove("--request")
            .ok_or_else(|| Error::new("missing --request"))?,
    )?;
    let file = options
        .remove("--file")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::new("missing --file"))?;
    let seconds = options
        .remove("--seconds")
        .unwrap_or("60")
        .parse::<u64>()
        .ok()
        .filter(|n| (1..=60).contains(n))
        .ok_or_else(|| Error::new("--seconds must be 1 through 60"))?;
    if !options.is_empty() || logical == [0; 16] {
        return Err(Error::new("invalid account request option or zero ID"));
    }
    Ok(Request {
        selection,
        logical,
        file,
        within: Duration::from_secs(seconds),
    })
}

fn run(words: &[String]) -> Result<Secret> {
    let request = parse(words)?;
    let input = fsx::read_private(Path::new(request.file), 64 * 1024)
        .map_err(|_| Error::new("request file must be a private owned regular file within 64 KiB"))?
        .ok_or_else(|| Error::new("request file is missing"))?;
    let paths = Paths::live().map_err(|_| Error::new("account request paths unavailable"))?;
    let config = client::config(&paths)
        .map_err(|_| Error::new("account request configuration unavailable"))?
        .ok_or_else(|| Error::new("account request needs a configured secrets vault"))?;
    let broker = Broker::at(paths, config)?;
    let deadline = Instant::now() + request.within;
    let payload = Secret::new(input.as_slice());
    let reply = match request.selection {
        super::Selection::Grant(id) => {
            google::request_until(broker, id, request.logical, payload, deadline)
        }
        super::Selection::Account { account, .. } => {
            google::request_account_until(broker, account, request.logical, payload, deadline)
        }
    }?;
    render(request.logical, reply)
}

fn render(logical: [u8; 16], reply: Reply) -> Result<Secret> {
    let status = match reply.status {
        Status::Refused => "refused",
        Status::Unknown => "unknown",
        Status::Completed => "completed",
        Status::Rejected => "rejected",
        Status::Unconfirmed => "unconfirmed",
    };
    let response = if reply.body.is_empty() {
        None
    } else {
        Some(
            google::response(reply.body.expose())
                .map_err(|_| Error::new("invalid private provider response"))?,
        )
    };
    let body_utf8 = response.map(|(_, body)| std::str::from_utf8(body));
    let mut out = Zeroizing::new(
        aterm_json::to_vec(&json!({
            "request": hex::encode_lower(&logical), "status": status, "entered": reply.entered,
            "unit": "requests", "http_status": response.map(|(status, _)| status),
            "body_encoding": if matches!(body_utf8, Some(Err(_))) { "hex" } else { "utf-8" },
        }))
        .map_err(|_| Error::new("request result encoding failed"))?,
    );
    out.pop(); // the metadata object is closed here; append the protected body
    // Reserve before private bytes enter this buffer, including the closing
    // bytes written after the body. Growing later can abandon an unwiped copy.
    out.reserve(response.map_or(4, |(_, body)| body.len() * 6 + 2) + 12);
    out.extend_from_slice(b",\"body\":");
    if let Some((_, body)) = response {
        match body_utf8.expect("response has utf8 check") {
            Ok(text) => {
                let encoded = Zeroizing::new(
                    aterm_json::quoted_string(text)
                        .map_err(|_| Error::new("request body encoding failed"))?,
                );
                out.extend_from_slice(&encoded);
            }
            Err(_) => {
                let text = Zeroizing::new(hex::encode_lower(body));
                out.push(b'"');
                out.extend_from_slice(text.as_bytes());
                out.push(b'"');
            }
        }
    } else {
        out.extend_from_slice(b"null");
    }
    out.extend_from_slice(b"}\n");
    Ok(Secret::new(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_request_parse_refuses_unbounded_or_ambiguous_invocations() {
        let base = [
            "--grant",
            "61616161616161616161616161616161",
            "--request",
            "62626262626262626262626262626262",
            "--file",
            "/private/request.json",
        ];
        let words = base.map(str::to_owned).to_vec();
        assert!(parse(&words).is_ok());
        let mut by_account = words.clone();
        by_account[0] = "--account".into();
        by_account[1] = "63".repeat(32);
        assert!(matches!(
            parse(&by_account).unwrap().selection,
            super::super::Selection::Account { .. }
        ));
        let mut both = by_account;
        both.extend(["--grant".into(), "61".repeat(16)]);
        assert!(parse(&both).is_err());
        for extra in [
            vec!["--seconds", "0"],
            vec!["--seconds", "61"],
            vec!["--file", "duplicate"],
            vec!["--url", "https://example.invalid"],
            vec!["--unknown"],
        ] {
            let mut bad = words.clone();
            bad.extend(extra.into_iter().map(str::to_owned));
            assert!(parse(&bad).is_err());
        }
        let mut zero = words;
        zero[3] = "00".repeat(16);
        assert!(parse(&zero).is_err());
    }

    #[test]
    fn account_request_output_keeps_private_bodies_escaped_and_states_explicit() {
        for body in [b"private \"answer\"\n".as_slice(), b"\xff"] {
            let mut bytes = vec![0, 200];
            bytes.extend_from_slice(body);
            let rendered = render(
                [98; 16],
                Reply {
                    status: Status::Unconfirmed,
                    entered: true,
                    body: Secret::new(&bytes),
                },
            )
            .unwrap();
            let value: aterm_json::Value = aterm_json::from_slice(rendered.expose()).unwrap();
            assert_eq!(value["http_status"].as_u64(), Some(200));
            assert_eq!(value["status"].as_str(), Some("unconfirmed"));
            if body == b"\xff" {
                assert_eq!(value["body_encoding"].as_str(), Some("hex"));
                assert_eq!(value["body"].as_str(), Some("ff"));
            } else {
                assert_eq!(value["body"].as_str(), std::str::from_utf8(body).ok());
            }
        }
        let answer = render(
            [98; 16],
            Reply {
                status: Status::Unknown,
                entered: false,
                body: Secret::new(b""),
            },
        )
        .unwrap();
        let value: aterm_json::Value = aterm_json::from_slice(answer.expose()).unwrap();
        assert!(value["body"].is_null());
        assert!(value["http_status"].is_null());
    }
}
