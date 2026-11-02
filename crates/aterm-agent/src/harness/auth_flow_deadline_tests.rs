// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    Deadline, HttpResponse, MailboxFlow, authorize_using_with_page_until, curl_args_until,
    curl_command, follow_until_page_or_callback_until,
};
use crate::harness::auth_deadline::EXPIRED;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const AUTHORIZE: &str = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9123%2Fcallback&state=deadline";
const MAGIC: &str = "https://claude.ai/magic?token=owned";
const CALLBACK: &str = "http://127.0.0.1:9123/callback?code=accepted&state=deadline";

fn response(status: u16, location: Option<&str>, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        location: location.map(str::to_owned),
        body: body.into(),
    }
}

#[test]
fn an_expired_flow_dispatches_neither_http_nor_a_credential_read() {
    let now = Instant::now();
    let clock = || now;
    let deadline = Deadline::new(Duration::ZERO, &clock);
    let requests = Cell::new(0);
    let credentials = Cell::new(0);
    let mut exchange = |_: &str, _: &str, _: &mut BTreeMap<String, String>, _: Option<&str>| {
        requests.set(requests.get() + 1);
        Ok(response(200, None, "unexpected"))
    };
    let result = authorize_using_with_page_until(
        AUTHORIZE,
        None,
        |_, _| {
            credentials.set(credentials.get() + 1);
            Ok(MAGIC.into())
        },
        &mut exchange,
        deadline,
    );
    assert_eq!(result, Err(EXPIRED));
    // The direct redirect walker must also refuse an otherwise valid callback.
    let flow = MailboxFlow::new(AUTHORIZE).unwrap();
    let callback = follow_until_page_or_callback_until(
        &flow,
        &mut exchange,
        "GET",
        CALLBACK,
        &mut BTreeMap::new(),
        None,
        deadline,
    );
    assert_eq!(callback.err(), Some(EXPIRED));
    assert_eq!(requests.get(), 0);
    assert_eq!(credentials.get(), 0);
}

#[test]
fn redirects_spend_one_budget_before_the_callback_can_be_dispatched() {
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let flow = MailboxFlow::new(AUTHORIZE).unwrap();
    let mut requests = Vec::new();
    let result = follow_until_page_or_callback_until(
        &flow,
        &mut |method, url, _, body| {
            requests.push((method.to_owned(), url.to_owned()));
            assert!(body.is_none());
            now.set(now.get() + Duration::from_secs(4));
            Ok(match requests.len() {
                1 => response(302, Some("/redirect-two"), ""),
                2 => response(302, Some("/redirect-three"), ""),
                3 => response(302, Some(CALLBACK), ""),
                _ => panic!("a redirect renewed the authorization budget"),
            })
        },
        "GET",
        AUTHORIZE,
        &mut BTreeMap::new(),
        None,
        deadline,
    );
    assert_eq!(result.err(), Some(EXPIRED));
    assert_eq!(
        requests,
        [
            ("GET".into(), AUTHORIZE.into()),
            ("GET".into(), "https://claude.ai/redirect-two".into()),
            ("GET".into(), "https://claude.ai/redirect-three".into()),
        ]
    );
}

#[test]
fn late_pages_credentials_and_forms_stop_the_flow_but_timely_approval_succeeds() {
    let first_form = "<form method=POST action=/approve><input type=hidden name=csrf value=one><button>Allow</button></form>";
    let last_form = "<form method=POST action=/confirm><input type=hidden name=csrf value=two><button>Allow</button></form>";
    let expected = [
        ("GET", AUTHORIZE, None),
        ("GET", MAGIC, None),
        ("POST", "https://claude.ai/approve", Some("csrf=one")),
        ("POST", "https://claude.ai/confirm", Some("csrf=two")),
        ("GET", CALLBACK, None),
    ];
    // Action 1 is the credential read; all the other actions are HTTP exchanges.
    for (late, request_count, credential_count) in [
        (Some(0), 1, 0),
        (Some(1), 1, 1),
        (Some(2), 2, 1),
        (Some(3), 3, 1),
        (Some(4), 4, 1),
        (Some(5), 5, 1),
        (None, 5, 1),
    ] {
        let start = Instant::now();
        let now = Cell::new(start);
        let clock = || now.get();
        let deadline = Deadline::new(Duration::from_secs(20), &clock);
        let advance = |action| {
            now.set(if late == Some(action) {
                deadline.end()
            } else {
                now.get() + Duration::from_secs(1)
            });
        };
        let credentials = Cell::new(0);
        let mut requests = Vec::new();
        let result = authorize_using_with_page_until(
            AUTHORIZE,
            Some("actor@example.test"),
            |email, page| {
                assert_eq!(email, Some("actor@example.test"));
                assert_eq!(page, "Check your email");
                credentials.set(credentials.get() + 1);
                advance(1);
                Ok(MAGIC.into())
            },
            &mut |method, url, _, body| {
                let index = requests.len();
                assert_eq!((method, url, body), expected[index]);
                requests.push((method.to_owned(), url.to_owned(), body.map(str::to_owned)));
                advance(if index == 0 { 0 } else { index + 1 });
                Ok(match index {
                    0 => response(200, None, "Check your email"),
                    1 => response(200, None, first_form),
                    2 => response(200, None, last_form),
                    3 => response(302, Some(CALLBACK), ""),
                    4 => response(200, None, "approved"),
                    _ => unreachable!(),
                })
            },
            deadline,
        );
        assert_eq!(result, if late.is_some() { Err(EXPIRED) } else { Ok(()) });
        assert_eq!(requests.len(), request_count, "late action {late:?}");
        assert_eq!(credentials.get(), credential_count, "late action {late:?}");
    }
}

#[test]
fn curl_transport_caps_shrink_and_never_encode_unlimited_zero() {
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let args = ["--disable", "--max-time", "30", "-sS", "--max-time", "99"];
    for (elapsed, ceiling) in [
        (Duration::ZERO, Duration::from_secs(3)),
        (Duration::from_millis(3250), Duration::from_secs(15)),
        (Duration::from_micros(9_998_500), Duration::from_secs(15)),
    ] {
        now.set(start + elapsed);
        let selected = curl_args_until(&args, ceiling, deadline).unwrap();
        assert_eq!(
            selected.iter().filter(|arg| *arg == "--max-time").count(),
            1
        );
        let index = selected.iter().position(|arg| arg == "--max-time").unwrap();
        let seconds = selected[index + 1].parse::<f64>().unwrap();
        let cap = Duration::from_secs_f64(seconds);
        assert!(cap > Duration::ZERO);
        assert!(cap <= deadline.remaining().unwrap());
        assert!(cap <= ceiling);
        let selected: Vec<_> = selected.iter().map(String::as_str).collect();
        let (command, _) = curl_command("curl", &selected, &[], &|_| None).unwrap();
        let actual: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&actual[..selected.len()], selected.as_slice());
        assert!(!actual.iter().any(|arg| arg == "30" || arg == "99"));
    }
    for elapsed in [Duration::from_micros(9_999_500), Duration::from_secs(10)] {
        now.set(start + elapsed);
        assert_eq!(
            curl_args_until(&args, Duration::from_secs(15), deadline)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
    }
}
