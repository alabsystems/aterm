// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Invoked by the existing current-root upkeep owner, never a second test registry.

use super::*;
use agh::wire::scripted::ScriptedTransport;
use aterm_json::{Value, json};

/// Production [`observe`] over a scripted provider that lists `rows` for the account the
/// saved `catalog` names. A classic `token` is answered with the `repo` scope and
/// whole-account counters that agree with `rows`, so the read proves the whole account; any
/// other token proves only what it sees.
pub(super) fn provider(
    token: SecretBytes,
    binding: Binding,
    catalog: &[u8],
    rows: &[Value],
    stopped: &AtomicBool,
) -> Result<Candidate, String> {
    let saved = RepositoryCatalog::parse(catalog).unwrap();
    let credential = CredentialBasis::of(&token);
    let transport = Arc::new(ScriptedTransport::new());
    let gh = github(token, transport.clone())?;
    let private = rows
        .iter()
        .filter(|row| row["private"].as_bool() == Some(true))
        .count();
    let (user, headers) = if credential.complete_token {
        (
            json!({"login":saved.account(),"id":saved.owner_id().unwrap(),"type":"User",
                "public_repos":rows.len() - private,"owned_private_repos":private}),
            vec![("x-oauth-scopes", "repo, read:user")],
        )
    } else {
        (
            json!({"login":saved.account(),"id":saved.owner_id().unwrap(),"type":"User",
                "public_repos":0}),
            Vec::new(),
        )
    };
    let user = aterm_json::to_vec(&user).unwrap();
    transport.answer(200, &headers, &user);
    transport.answer(200, &[], &aterm_json::to_vec(&rows).unwrap());
    transport.answer(200, &headers, &user);
    observe(
        &gh,
        binding,
        catalog,
        stopped,
        Instant::now() + FETCH,
        &credential,
    )
}

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
        // The counts a publication's line carries are this read's own: the listed row is
        // new, and only a whole-account read retires the saved ones it did not list.
        assert_eq!(
            candidate.changes,
            RepositoryCatalogChanges {
                added: 1,
                removed: if complete {
                    old["repositories"].as_array().unwrap().len()
                } else {
                    0
                },
                ..RepositoryCatalogChanges::default()
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
    // A saved repository listed with its size moved. A read of it unmoved answers the saved
    // catalog itself: the saved keys the listed row carries are no part of the catalog's
    // meaning. Inside the class of its size (at most 1 MiB for this one, saved at 1 KiB) a
    // read answers that catalog and no count; one KiB past it, that size's class alone.
    let saved = &old["repositories"][0];
    let size = saved["size"].as_u64().unwrap();
    assert_eq!(size, 1, "CONTROL: the saved size is in the least class");
    for (moved, size_class) in [(0, 0), (1_023, 0), (1_024, 1)] {
        let mut listed = saved.clone();
        listed
            .as_object_mut()
            .unwrap()
            .insert("size".into(), (size + moved).into());
        let candidate = provider(
            input.token.clone(),
            input.binding.clone(),
            &input.catalog,
            &[listed],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            candidate.changes,
            RepositoryCatalogChanges {
                size_class,
                ..RepositoryCatalogChanges::default()
            },
            "a size moved by {moved} KiB"
        );
        assert_eq!(
            aterm_json::from_slice::<Value>(&candidate.bytes).unwrap() == old,
            size_class == 0,
            "a size moved by {moved} KiB"
        );
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
    // GitHub lists a saved repository whole, beside a repository new to the catalog, so that
    // the catalog is published. The row here carries every key the saved object holds but its
    // policy, and the read hands it to the refresh as listed: the candidate's object for that
    // repository holds what a refresh writes and the policy, nothing else.
    let newcomer = row;
    let rows = old["repositories"].as_array().unwrap();
    let saved = rows
        .iter()
        .find(|repo| repo.get("required_submodules").is_some())
        .expect("a saved repository with policy");
    let mut listed = saved.clone();
    listed
        .as_object_mut()
        .unwrap()
        .remove("required_submodules");
    let keys =
        |object: &Value| -> Vec<String> { object.as_object().unwrap().keys().cloned().collect() };
    let lean = [
        "archived",
        "clone_url",
        "default_branch",
        "fork",
        "full_name",
        "id",
        "name",
        "owner",
        "private",
        "required_submodules",
        "size",
    ];
    assert!(
        keys(saved).len() > lean.len() && keys(&saved["owner"]).len() > 2,
        "the saved repository holds no key beside what a refresh writes"
    );
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
        // Two private repositories listed, so a whole-account read counts two.
        let mut both = user(complete);
        if complete {
            both.as_object_mut()
                .unwrap()
                .insert("owned_private_repos".into(), 2.into());
        }
        transport.answer(200, &headers, &aterm_json::to_vec(&both).unwrap());
        transport.answer(
            200,
            &[],
            &aterm_json::to_vec(&json!([listed.clone(), newcomer.clone()])).unwrap(),
        );
        transport.answer(200, &headers, &aterm_json::to_vec(&both).unwrap());
        let candidate = observe(
            &gh,
            input.binding.clone(),
            &input.catalog,
            &AtomicBool::new(false),
            Instant::now() + FETCH,
            &credential,
        )
        .unwrap();
        let value: Value = aterm_json::from_slice(&candidate.bytes).unwrap();
        let after = value["repositories"].as_array().unwrap();
        let row = after
            .iter()
            .find(|row| row["id"] == saved["id"])
            .expect("the listed repository");
        assert_eq!(
            keys(row),
            lean,
            "the read did not hand the refresh GitHub's row whole"
        );
        assert_eq!(keys(&row["owner"]), ["id", "login"]);
        assert_eq!(row["required_submodules"], saved["required_submodules"]);
        // A whole-account read retires the saved repositories it did not list; a token's
        // read leaves each exactly as saved.
        assert_eq!(
            candidate.changes,
            RepositoryCatalogChanges {
                added: 1,
                pruned: 1,
                removed: if complete { rows.len() - 1 } else { 0 },
                ..RepositoryCatalogChanges::default()
            }
        );
        assert_eq!(after.len(), if complete { 2 } else { rows.len() + 1 });
        if !complete {
            for previous in rows.iter().filter(|repo| repo["id"] != saved["id"]) {
                assert!(
                    after.contains(previous),
                    "a token's read changed a saved repository it did not list"
                );
            }
        }
    }
}
