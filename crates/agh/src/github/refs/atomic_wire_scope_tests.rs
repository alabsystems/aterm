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
fn the_wire_capability_cannot_move_to_other_bytes_paths_origins_methods_or_permits() {
    let root = crate::test_fixtures::scratch_dir("atomic-scope");
    let text = crate::test_fixtures::canonical_text(root.path());
    let parsed = crate::config::parse(&text).expect("config");
    let valid =
        crate::config::validate(&parsed, root.path(), &crate::config::file_id).expect("config");
    let router = Router::from_validated(valid);
    let slug = Slug::parse("fixture-dev/app").expect("repo");
    let make = |slug: Slug| {
        router
            .permit(&RouteRequest::write(RouteTarget::Repo(slug)), &|_| {
                Presence::Present
            })
            .expect("permit")
    };
    let permit = make(slug.clone());
    let other = make(Slug::parse("fixture-dev/other").expect("repo"));
    let fake = agh_fake::FakeGithub::start();
    let p = fake.ports();
    let endpoints = Endpoints::loopback(p.api, p.uploads, p.web, p.cdn);
    let credential = Credential::for_tests("dev", Tier::Dev, agh_fake::token("dev").as_bytes());
    let api_auth = AuthHeader::for_origin(&credential, &endpoints, endpoints.api()).expect("auth");
    let uploads_auth =
        AuthHeader::for_origin(&credential, &endpoints, endpoints.uploads()).expect("auth");
    let body = b"validated request bytes";
    let scope = AtomicRefWrite {
        slug,
        api: endpoints.api().clone(),
        body: Sha256Digest::of(body),
    };
    let transport = crate::wire::http::HttpTransport::direct(Arc::new(MapEnv::new()));
    for case in [
        "body",
        "query",
        "path",
        "origin",
        "permit",
        "method",
        "missing-scope",
        "reader",
        "no-auth",
    ] {
        let mut request = WireRequest::new(
            Method::Post,
            format!("{}/graphql", endpoints.api().base()),
            Vec::new(),
            1024,
            Deadlines::JSON,
        );
        request.auth = Some(&api_auth);
        request.permit = Some(&permit);
        request.atomic_refs = Some(&scope);
        request.body = WireBody::Bytes(body);
        let mut reader = std::io::Cursor::new(body);
        match case {
            "body" => request.body = WireBody::Bytes(b"different bytes"),
            "query" => request.url.push_str("?query=another"),
            "path" => {
                request.url = format!("{}/repos/fixture-dev/app/git/refs", endpoints.api().base())
            }
            "origin" => {
                request.url = format!("{}/graphql", endpoints.uploads().base());
                request.auth = Some(&uploads_auth);
            }
            "permit" => request.permit = Some(&other),
            "method" => request.method = Method::Patch,
            "missing-scope" => request.atomic_refs = None,
            "reader" => {
                request.body = WireBody::Reader {
                    len: body.len() as u64,
                    reader: &mut reader,
                }
            }
            "no-auth" => request.auth = None,
            _ => unreachable!(),
        }
        let error = match transport.round_trip(request) {
            Err(error) => error,
            Ok(_) => panic!("the transport sent {case}"),
        };
        assert_eq!(error.phase, Phase::NotSent, "{case}");
    }
    assert!(fake.seen().is_empty());
}
