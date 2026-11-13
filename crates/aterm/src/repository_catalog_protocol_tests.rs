// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Invoked by the existing current-root upkeep owner, never a second test registry.

use super::*;
use agh::wire::scripted::ScriptedTransport;
use aterm_json::{Value, json};

pub(super) fn exercise(input: &Input) {
    let old: Value = aterm_json::from_slice(&input.catalog).unwrap();
    let owner = old["account"].as_str().unwrap();
    let owner_id = RepositoryCatalog::parse(&input.catalog)
        .unwrap()
        .owner_id()
        .unwrap();
    let user = |complete: bool| {
        let mut value = json!({"login":owner,"id":owner_id,"type":"User","public_repos":0});
        if complete {
            value
                .as_object_mut()
                .unwrap()
                .insert("owned_private_repos".into(), 1.into());
        }
        value
    };
    let row = json!({"id":99191,"name":"new-owned","owner":{"login":owner,"id":owner_id},
        "full_name":format!("{owner}/new-owned"),"private":true,"fork":false,"archived":false,
        "size":1,"default_branch":"main","clone_url":format!("https://github.com/{owner}/new-owned.git")});
    for complete in [false, true] {
        let transport = Arc::new(ScriptedTransport::new());
        let token = if complete {
            SecretBytes::from(b"ghp_fixture_classic_only_test".as_slice())
        } else {
            input.token.clone()
        };
        let credential = CredentialBasis::of(&token);
        let gh = github(token, transport.clone()).unwrap();
        let headers = if complete {
            vec![("x-oauth-scopes", "repo, read:user")]
        } else {
            Vec::new()
        };
        transport.answer(200, &headers, &aterm_json::to_vec(&user(complete)).unwrap());
        transport.answer(
            200,
            &[],
            &aterm_json::to_vec(&json!([row.clone()])).unwrap(),
        );
        transport.answer(200, &headers, &aterm_json::to_vec(&user(complete)).unwrap());
        let candidate = observe(
            &gh,
            input.binding.clone(),
            &input.catalog,
            &AtomicBool::new(false),
            Instant::now() + FETCH,
            &credential,
        )
        .unwrap();
        assert_eq!(
            candidate.visibility,
            if complete {
                RepositoryCatalogVisibility::AccountComplete
            } else {
                RepositoryCatalogVisibility::TokenVisible
            }
        );
        let value: Value = aterm_json::from_slice(&candidate.bytes).unwrap();
        assert_eq!(
            value["repositories"].as_array().unwrap().len(),
            if complete {
                1
            } else {
                old["repositories"].as_array().unwrap().len() + 1
            }
        );
        if !complete {
            for previous in old["repositories"].as_array().unwrap() {
                assert!(
                    value["repositories"].as_array().unwrap().contains(previous),
                    "restricted visibility retains each unseen complete saved object"
                );
            }
        }
        let requests = transport.requests();
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.auth_profile.as_deref() == Some(PROFILE))
        );
        assert!(
            requests
                .iter()
                .all(|request| request.auth_fingerprint == requests[0].auth_fingerprint)
        );
        assert!(requests[1].url.contains("affiliation=owner"));
    }
    // Every bad complete page/read retains the catalog; none can mint even an additive result.
    for fault in [
        "wrong-user",
        "wrong-owner",
        "duplicate",
        "missing-private",
        "counters",
        "changed-user",
        "foreign-next",
        "capped",
        "torn",
        "stopped",
    ] {
        let transport = Arc::new(ScriptedTransport::new());
        let token = SecretBytes::from(b"ghp_fixture_classic_only_test".as_slice());
        let credential = CredentialBasis::of(&token);
        let gh = github(token, transport.clone()).unwrap();
        let headers = [("x-oauth-scopes", "repo, read:user")];
        let mut first = user(true);
        if fault == "wrong-user" {
            first
                .as_object_mut()
                .unwrap()
                .insert("id".into(), (owner_id + 1).into());
        }
        if fault == "counters" {
            first
                .as_object_mut()
                .unwrap()
                .insert("owned_private_repos".into(), 2.into());
        }
        transport.answer(200, &headers, &aterm_json::to_vec(&first).unwrap());
        let mut changed = row.clone();
        if fault == "wrong-owner" {
            changed
                .as_object_mut()
                .unwrap()
                .get_mut("owner")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("id".into(), (owner_id + 1).into());
        }
        if fault == "missing-private" {
            changed.as_object_mut().unwrap().remove("private");
        }
        let rows = if fault == "duplicate" {
            json!([changed.clone(), changed])
        } else {
            json!([changed])
        };
        let body = aterm_json::to_vec(&rows).unwrap();
        if fault == "capped" {
            for page in 1..=20 {
                let next = format!(
                    "<https://api.github.com/user/repos?affiliation=owner&per_page=100&page={}>; rel=\"next\"",
                    page + 1
                );
                let mut page_row = row.clone();
                page_row
                    .as_object_mut()
                    .unwrap()
                    .insert("id".into(), (99191 + page).into());
                transport.answer(
                    200,
                    &[("link", next.as_str())],
                    &aterm_json::to_vec(&json!([page_row])).unwrap(),
                );
            }
        } else if fault == "foreign-next" {
            transport.answer(
                200,
                &[(
                    "link",
                    "<https://elsewhere.invalid/user/repos?page=2>; rel=\"next\"",
                )],
                &body,
            );
        } else if fault == "torn" {
            transport.answer_body_fault(200, &[], &body, agh::wire::WireKind::Reset);
        } else {
            transport.answer(200, &[], &body);
        }
        let mut last = first.clone();
        if fault == "changed-user" {
            last.as_object_mut()
                .unwrap()
                .insert("id".into(), (owner_id + 1).into());
        }
        transport.answer(200, &headers, &aterm_json::to_vec(&last).unwrap());
        assert!(
            observe(
                &gh,
                input.binding.clone(),
                &input.catalog,
                &AtomicBool::new(fault == "stopped"),
                Instant::now() + FETCH,
                &credential
            )
            .is_err(),
            "{fault}"
        );
        assert!(
            transport
                .requests()
                .iter()
                .all(|request| request.auth_profile.as_deref() == Some(PROFILE))
        );
    }
}
