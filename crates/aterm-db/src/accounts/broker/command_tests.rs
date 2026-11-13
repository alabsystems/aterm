// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
