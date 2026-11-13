// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixture::{Server, cut_short, json};
use super::*;

const TOKEN: &[u8] = b"fixture_machine_user_token_only";

fn loopback() -> Client {
    Client::new(Trust::Roots(Vec::new()), ProxyMode::Direct, BUDGET)
}

fn token() -> Secret {
    Secret::new(TOKEN).unwrap()
}

fn page(server: &Server, page: usize) -> Result<crate::github_pages::Page, String> {
    let reply = send(
        &loopback(),
        &server.origin,
        &token(),
        RestMethod::Get,
        &format!("/user/keys?per_page=2&page={page}"),
        None,
    )?;
    crate::github_pages::page(reply.status, reply.headers, &reply.body)
}

const NEXT: &str = "<https://api.github.com/user/keys?per_page=2&page=2>; rel=\"next\", <https://api.github.com/user/keys?per_page=2&page=2>; rel=\"last\"";

/// Two pages read whole over a real socket, the token in the Authorization header alone.
#[test]
fn pagination_reads_every_page_and_a_page_cut_short_is_refused() {
    let server = Server::start(vec![
        json(200, &[("Link", NEXT)], r#"[{"id":1},{"id":2}]"#),
        json(200, &[], r#"[{"id":3}]"#),
    ]);
    let items = crate::github_pages::all(
        crate::github_pages::API,
        "/user/keys?per_page=2",
        2,
        |endpoint| {
            let page_no = endpoint.rsplit_once("page=").unwrap().1.parse().unwrap();
            page(&server, page_no)
        },
    )
    .unwrap();
    assert_eq!(items.len(), 3);
    let seen = server.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].line, "GET /user/keys?per_page=2&page=2 HTTP/1.1");
    let bearer = format!("Bearer {}", std::str::from_utf8(TOKEN).unwrap());
    assert!(
        seen.iter()
            .all(|s| s.header("authorization") == Some(bearer.as_str()))
    );
    assert!(
        seen.iter()
            .all(|s| s.line.as_bytes().windows(TOKEN.len()).all(|w| w != TOKEN))
    );
    assert_eq!(seen[0].header("x-github-api-version"), Some(API_VERSION));

    let server = Server::start(vec![
        json(200, &[("Link", NEXT)], r#"[{"id":1},{"id":2}]"#),
        cut_short(r#"[{"id":3},"#, 9),
    ]);
    let why = crate::github_pages::all(
        crate::github_pages::API,
        "/user/keys?per_page=2",
        2,
        |endpoint| {
            let page_no = endpoint.rsplit_once("page=").unwrap().1.parse().unwrap();
            page(&server, page_no)
        },
    )
    .unwrap_err();
    assert!(
        why.starts_with("GitHub GET /user/keys?per_page=2&page=2: "),
        "{why}"
    );
    assert!(!why.contains(std::str::from_utf8(TOKEN).unwrap()), "{why}");
}

/// A redirect is refused and never followed: the listener sees one request, and the
/// place it named sees none. Negative control: the same request answered 200 is read.
#[test]
fn a_redirect_is_refused_and_never_followed() {
    let elsewhere = Server::start(vec![json(200, &[], "{}")]);
    for status in [301, 302, 307, 308] {
        let location = format!("{}/user", elsewhere.origin);
        let moved = json(status, &[("Location", &location)], "{}");
        let server = Server::start(vec![moved.clone(), moved]);
        for (method, body) in [
            (RestMethod::Get, None),
            (RestMethod::Post, Some(&b"{}"[..])),
        ] {
            let why =
                send(&loopback(), &server.origin, &token(), method, "/user", body).unwrap_err();
            assert!(why.contains("a redirect"), "{why}");
        }
        assert_eq!(server.seen().len(), 2, "one request each, none repeated");
    }
    assert!(
        elsewhere.seen().is_empty(),
        "the redirect target was never asked"
    );
    let server = Server::start(vec![json(200, &[], r#"{"login":"fleetbot"}"#)]);
    let reply = send(
        &loopback(),
        &server.origin,
        &token(),
        RestMethod::Get,
        "/user",
        None,
    )
    .unwrap();
    assert_eq!(
        (reply.status, reply.body.as_slice()),
        (200, &br#"{"login":"fleetbot"}"#[..])
    );
}

#[test]
fn a_write_carries_its_json_body_and_a_delete_none() {
    let server = Server::start(vec![json(201, &[], r#"{"id":9}"#), json(204, &[], "")]);
    let body = br#"{"title":"t","key":"k"}"#;
    let created = send(
        &loopback(),
        &server.origin,
        &token(),
        RestMethod::Post,
        "/user/keys",
        Some(body),
    )
    .unwrap();
    assert_eq!(created.status, 201);
    let deleted = send(
        &loopback(),
        &server.origin,
        &token(),
        RestMethod::Delete,
        "/user/keys/9",
        None,
    )
    .unwrap();
    assert_eq!(deleted.status, 204);
    let seen = server.seen();
    assert_eq!(seen[0].line, "POST /user/keys HTTP/1.1");
    assert_eq!(seen[0].body, body);
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[1].line, "DELETE /user/keys/9 HTTP/1.1");
    assert!(seen[1].body.is_empty());
}

/// Each endpoint that is not one path is refused by the guard itself, before a connection:
/// a listener that answers any request sees none. Negative control: the same listener
/// answers a plain path, so a request that got past the guard would show.
#[test]
fn an_endpoint_that_is_not_one_path_is_refused_before_any_connection() {
    let refused = [
        "https://elsewhere.invalid/user",
        "//elsewhere.invalid/user",
        "/user\n",
        "/user\r\nX-Injected: 1",
        "/user keys",
        "/user#fragment",
        "/\\elsewhere.invalid/user",
        "/user/\u{e9}",
        "user",
        "",
    ];
    let server = Server::start(vec![json(200, &[], "{}"); refused.len() + 1]);
    for endpoint in refused {
        let why = send(
            &loopback(),
            &server.origin,
            &token(),
            RestMethod::Get,
            endpoint,
            None,
        )
        .unwrap_err();
        assert_eq!(why, "invalid GitHub API endpoint", "{endpoint:?}");
    }
    assert!(server.seen().is_empty(), "{:?}", server.seen());
    let reply = send(
        &loopback(),
        &server.origin,
        &token(),
        RestMethod::Get,
        "/user",
        None,
    )
    .unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(server.seen().len(), 1);
}
