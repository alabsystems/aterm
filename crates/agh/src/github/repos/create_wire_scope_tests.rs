// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use crate::identity::{Credential, MapEnv};
use crate::policy::{Presence, Router, Tier};
use crate::wire::door::AuthHeader;
use crate::wire::{Deadlines, Endpoints, Phase, Transport, WireRequest};
use std::sync::Arc;

#[test]
fn creation_scope_cannot_change_destination_payload_method_or_origin() {
    let root = crate::test_fixtures::scratch_dir("create-scope");
    let text = crate::test_fixtures::canonical_text(root.path());
    let config = crate::config::parse(&text).unwrap();
    let valid = crate::config::validate(&config, root.path(), &crate::config::file_id).unwrap();
    let router = Router::from_validated(valid);
    let make = |slug: &str| {
        router
            .permit(
                &RouteRequest::write(RouteTarget::Repo(Slug::parse(slug).unwrap())),
                &|_| Presence::Present,
            )
            .unwrap()
    };
    let permit = make("fixture-dev/app");
    let other = make("fixture-dev/other");
    let fake = agh_fake::FakeGithub::start();
    let p = fake.ports();
    let endpoints = Endpoints::loopback(p.api, p.uploads, p.web, p.cdn);
    let credential = Credential::for_tests("dev", Tier::Dev, agh_fake::token("dev").as_bytes());
    let api_auth = AuthHeader::for_origin(&credential, &endpoints, endpoints.api()).unwrap();
    let uploads_auth =
        AuthHeader::for_origin(&credential, &endpoints, endpoints.uploads()).unwrap();
    let body = br#"{"auto_init":false,"name":"app","private":false}"#;
    let scope = RepoCreateWrite {
        slug: Slug::parse("fixture-dev/app").unwrap(),
        api: endpoints.api().clone(),
        body: Sha256Digest::of(body),
    };
    let transport = crate::wire::http::HttpTransport::direct(Arc::new(MapEnv::new()));
    for case in [
        "private",
        "name",
        "query",
        "path",
        "origin",
        "permit",
        "method",
        "missing-scope",
        "reader",
        "no-auth",
    ] {
        let mut req = WireRequest::new(
            Method::Post,
            format!("{}/user/repos", endpoints.api().base()),
            Vec::new(),
            1024,
            Deadlines::JSON,
        );
        req.auth = Some(&api_auth);
        req.permit = Some(&permit);
        req.repo_create = Some(&scope);
        req.body = WireBody::Bytes(body);
        let mut reader = std::io::Cursor::new(body);
        match case {
            "private" => {
                req.body = WireBody::Bytes(br#"{"auto_init":false,"name":"app","private":true}"#)
            }
            "name" => {
                req.body = WireBody::Bytes(br#"{"auto_init":false,"name":"other","private":false}"#)
            }
            "query" => req.url.push_str("?owner=other"),
            "path" => req.url = format!("{}/user/keys", endpoints.api().base()),
            "origin" => {
                req.url = format!("{}/user/repos", endpoints.uploads().base());
                req.auth = Some(&uploads_auth);
            }
            "permit" => req.permit = Some(&other),
            "method" => req.method = Method::Patch,
            "missing-scope" => req.repo_create = None,
            "reader" => {
                req.body = WireBody::Reader {
                    len: body.len() as u64,
                    reader: &mut reader,
                }
            }
            "no-auth" => req.auth = None,
            _ => unreachable!(),
        }
        let error = match transport.round_trip(req) {
            Err(error) => error,
            Ok(_) => panic!("the transport sent {case}"),
        };
        assert_eq!(error.phase, Phase::NotSent, "{case}");
    }
    assert!(fake.seen().is_empty());
}
