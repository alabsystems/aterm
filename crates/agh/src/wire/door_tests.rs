// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! In-crate tests for `AuthHeader::for_origin` across endpoint and origin configurations.

use super::AuthHeader;
use crate::identity::Credential;
use crate::policy::Tier;
use crate::wire::{Endpoints, Origin};

const PORT: u16 = 47_001;

fn origin(scheme: &'static str, host: &str, port: u16) -> Origin {
    Origin {
        scheme,
        host: host.to_string(),
        port,
    }
}

fn endpoints_with(api: Origin) -> Endpoints {
    Endpoints {
        uploads: api.clone(),
        api,
        web: Origin::https("github.com"),
        asset_hosts: vec![Origin::https("objects.githubusercontent.com")],
    }
}

fn cred() -> Credential {
    Credential::for_tests("dev", Tier::Dev, agh_fake::token("door").as_bytes())
}

#[test]
fn a_mirror_gets_no_header() {
    for mirror in [
        origin("http", "mirror.example", 80),
        origin("https", "mirror.example", 443),
    ] {
        let endpoints = endpoints_with(mirror.clone());
        assert!(AuthHeader::for_origin(&cred(), &endpoints, &mirror).is_none());
        assert!(AuthHeader::for_origin(&cred(), &endpoints, endpoints.uploads()).is_none());
    }
}

#[test]
fn githubs_hosts_off_tls_get_no_header() {
    for api in [
        origin("http", "api.github.com", 80),
        origin("https", "api.github.com", 8443),
    ] {
        let endpoints = endpoints_with(api.clone());
        assert!(AuthHeader::for_origin(&cred(), &endpoints, &api).is_none());
    }
}

#[test]
fn hosts_that_are_not_loopback_get_no_header() {
    for api in [
        origin("http", "localhost", PORT),
        origin("http", "127.0.0.2", PORT),
        origin("https", "127.0.0.1", PORT),
    ] {
        let endpoints = endpoints_with(api.clone());
        assert!(AuthHeader::for_origin(&cred(), &endpoints, &api).is_none());
    }
}

#[test]
fn loopback_gets_a_header_in_the_test_build() {
    let api = origin("http", "127.0.0.1", PORT);
    let endpoints = endpoints_with(api.clone());
    let header = AuthHeader::for_origin(&cred(), &endpoints, &api);
    assert_eq!(header.as_ref().map(AuthHeader::origin), Some(&api));
}

#[test]
fn githubs_host_that_is_not_this_clients_origin_gets_no_header() {
    let endpoints = endpoints_with(origin("http", "mirror.example", 80));
    let github = Origin::https("api.github.com");
    assert!(AuthHeader::for_origin(&cred(), &endpoints, &github).is_none());
}
