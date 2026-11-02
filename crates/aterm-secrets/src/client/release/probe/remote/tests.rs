// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::release::probe::model::{Artifact, Repository, Subject};
use std::cell::RefCell;
use std::collections::VecDeque;

struct Fake {
    replies: RefCell<VecDeque<std::result::Result<Answer, ApiError>>>,
    methods: RefCell<Vec<Method>>,
}
impl Api for Fake {
    fn request(
        &self,
        method: Method,
        _: &str,
        _: Option<&Value>,
    ) -> std::result::Result<Answer, ApiError> {
        self.methods.borrow_mut().push(method);
        self.replies
            .borrow_mut()
            .pop_front()
            .expect("scripted request")
    }
    fn upload(&self, _: &str, _: &Path) -> std::result::Result<Answer, ApiError> {
        panic!("no upload in this fixture")
    }
    fn download(&self, _: &str, _: &Path, _: u64) -> Result<()> {
        panic!("no download in this fixture")
    }
}

fn plan() -> Journal {
    let mut repo = Repository::new("fixture-owner".into());
    repo.commit = Some("ab".repeat(20));
    repo.principal = Some("fixture-writer".into());
    repo.tag_owned = true;
    Journal {
        nonce: "12".repeat(16),
        subject: Subject {
            uid: 501,
            node: "34".repeat(16),
            member: "56".repeat(16),
            writer: "78".repeat(32),
            incarnation: "90".repeat(16),
            network: "ab".repeat(16),
            authority: "cd".repeat(32),
            genesis: "ef".repeat(32),
            vault: "01".repeat(32),
        },
        repositories: vec![repo],
        created_ms: 1,
        updated_ms: 1,
        artifact: Some(Artifact {
            sha256: "23".repeat(32),
            executable_sha256: "34".repeat(32),
            bytes: 100,
            signing_identity: "45".repeat(20),
            team: aterm_update_core::pins::APPLE_TEAM_ID.into(),
            executed: true,
        }),
        submission: Some("01234567-89ab-cdef-0123-456789abcdef".into()),
        apple_accepted: true,
        pending: None,
        cleanup_only: false,
    }
}
fn draft(remote: &Remote<'_>) -> Value {
    json!({"id":1,"draft":true,"prerelease":true,"tag_name":remote.journal.tag(),
        "name":remote.name(),"body":remote.marker(),"target_commitish":remote.journal.repositories[0].commit,
        "author":{"login":"fixture-writer"},"published_at":null,"assets":[]})
}
fn fake(replies: Vec<std::result::Result<Answer, ApiError>>) -> Fake {
    Fake {
        replies: RefCell::new(replies.into()),
        methods: RefCell::new(Vec::new()),
    }
}

fn lookup(api: &Fake, rows: Value) {
    api.replies.borrow_mut().extend([
        Ok(Answer {
            status: 404,
            body: Value::Null,
        }),
        Ok(Answer {
            status: 200,
            body: rows.clone(),
        }),
        Ok(Answer {
            status: 200,
            body: rows,
        }),
    ]);
}

fn replace(value: &mut Value, field: &str, replacement: Value) {
    let Value::Object(object) = value else {
        panic!("fixture object");
    };
    assert!(object.insert(field.to_owned(), replacement).is_some());
}

fn two_passes(api: &Fake, by_tag: Option<Value>, first: Value, second: Value) {
    api.replies.borrow_mut().extend([
        Ok(Answer {
            status: if by_tag.is_some() { 200 } else { 404 },
            body: by_tag.unwrap_or(Value::Null),
        }),
        Ok(Answer {
            status: 200,
            body: first,
        }),
        Ok(Answer {
            status: 200,
            body: second,
        }),
    ]);
}

#[test]
fn unrelated_downloads_do_not_prevent_a_new_draft() {
    let plan = plan();
    let api = fake(vec![]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    let old = json!({"id":99,"tag_name":"v0.147.0","updated_at":"old",
        "assets":[{"id":100,"download_count":0}]});
    let new = json!({"id":99,"tag_name":"v0.147.0","updated_at":"new",
        "assets":[{"id":100,"download_count":1}]});
    two_passes(&api, None, json!([old]), json!([new]));
    api.replies.borrow_mut().push_back(Ok(Answer {
        status: 201,
        body: draft(&remote),
    }));
    assert!(matches!(
        remote.create_draft().unwrap(),
        Outcome::Applied(Receipt::Draft(1))
    ));
    assert_eq!(
        *api.methods.borrow(),
        [Method::Get, Method::Get, Method::Get, Method::Post]
    );
}

#[test]
fn target_counters_and_listing_order_do_not_change_ownership() {
    let plan = plan();
    let api = fake(vec![]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    let mut first = draft(&remote);
    replace(
        &mut first,
        "assets",
        json!([{"id":2,"name":"owned","download_count":0}]),
    );
    let mut second = first.clone();
    replace(
        &mut second,
        "assets",
        json!([{"id":2,"name":"owned","download_count":1}]),
    );
    let unrelated = json!({"id":99,"tag_name":"v0.147.0"});
    two_passes(
        &api,
        Some(second.clone()),
        json!([first, unrelated.clone()]),
        json!([unrelated, second]),
    );
    assert_eq!(remote.objects_for_tag().unwrap().len(), 1);
}

#[test]
fn target_mutations_and_lookup_disagreement_refuse_before_post() {
    let plan = plan();
    for by_tag in [false, true] {
        for (field, replacement) in [
            ("id", json!(8)),
            ("tag_name", json!("other")),
            ("name", json!("other")),
            ("body", json!("other")),
            ("target_commitish", json!("other")),
            ("draft", json!(false)),
            ("prerelease", json!(false)),
            ("published_at", json!("published")),
            ("author", json!({"login":"other"})),
            ("assets", json!([{"id":9}])),
        ] {
            let api = fake(vec![]);
            let remote = Remote {
                api: &api,
                journal: &plan,
                index: 0,
                principal: "fixture-writer",
            };
            let first = draft(&remote);
            let mut changed = first.clone();
            replace(&mut changed, field, replacement);
            if by_tag {
                two_passes(&api, Some(changed), json!([first.clone()]), json!([first]));
            } else {
                two_passes(&api, None, json!([first]), json!([changed]));
            }
            assert!(
                remote.objects_for_tag().is_err(),
                "{field}, by_tag={by_tag}"
            );
            assert!(
                api.methods
                    .borrow()
                    .iter()
                    .all(|method| *method == Method::Get)
            );
        }
    }
}

#[test]
fn malformed_and_duplicate_listing_identities_refuse() {
    let plan = plan();
    for rows in [
        json!([{"id":0,"tag_name":"v1"}]),
        json!([{"id":1}]),
        json!([{"id":1,"tag_name":""}]),
        json!([{"id":"1","tag_name":"v1"}]),
        json!([{"id":1,"tag_name":"v1"},{"id":1,"tag_name":"v1"}]),
    ] {
        let api = fake(vec![]);
        let remote = Remote {
            api: &api,
            journal: &plan,
            index: 0,
            principal: "fixture-writer",
        };
        two_passes(&api, None, rows.clone(), rows);
        assert!(matches!(
            remote.create_draft().unwrap(),
            Outcome::NotStarted(_)
        ));
        assert!(
            api.methods
                .borrow()
                .iter()
                .all(|method| *method == Method::Get)
        );
    }
}

#[test]
fn target_asset_material_and_uploader_changes_refuse() {
    let plan = plan();
    for (field, replacement) in [
        ("id", json!(3)),
        ("name", json!("other")),
        ("label", json!("other")),
        ("state", json!("starter")),
        ("size", json!(101)),
        ("digest", json!("sha256:other")),
        ("uploader", json!({"login":"other"})),
    ] {
        let api = fake(vec![]);
        let remote = Remote {
            api: &api,
            journal: &plan,
            index: 0,
            principal: "fixture-writer",
        };
        let asset = json!({"id":2,"name":"owned","label":"nonce","state":"uploaded",
            "size":100,"digest":"sha256:exact","uploader":{"login":"fixture-writer"}});
        let mut changed_asset = asset.clone();
        replace(&mut changed_asset, field, replacement);
        let mut first = draft(&remote);
        replace(&mut first, "assets", json!([asset]));
        let mut second = first.clone();
        replace(&mut second, "assets", json!([changed_asset]));
        two_passes(&api, None, json!([first]), json!([second]));
        assert!(remote.objects_for_tag().is_err(), "{field}");
    }
}

#[test]
fn a_repeated_identity_across_pages_is_not_a_complete_observation() {
    let plan = plan();
    let api = fake(vec![
        Ok(Answer {
            status: 404,
            body: Value::Null,
        }),
        Ok(Answer {
            status: 200,
            body: Value::Array(
                (1..=100)
                    .map(|id| json!({"id":id,"tag_name":format!("v{id}")}))
                    .collect(),
            ),
        }),
        Ok(Answer {
            status: 200,
            body: json!([{"id":100,"tag_name":"v100"}]),
        }),
    ]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    assert!(matches!(
        remote.create_draft().unwrap(),
        Outcome::NotStarted(_)
    ));
    assert_eq!(*api.methods.borrow(), [Method::Get; 3]);
}

#[test]
fn a_lost_draft_reply_is_observed_without_a_second_post() {
    let plan = plan();
    let api = fake(vec![]);
    lookup(&api, json!([]));
    api.replies.borrow_mut().push_back(Err(ApiError::Uncertain));
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    let body = draft(&remote);
    assert!(matches!(
        remote.create_draft().unwrap(),
        Outcome::Pending(_)
    ));
    lookup(&api, json!([body]));
    assert!(matches!(
        remote.observe_draft().unwrap(),
        Outcome::Applied(Receipt::Draft(1))
    ));
    assert_eq!(
        api.methods
            .borrow()
            .iter()
            .filter(|method| **method == Method::Post)
            .count(),
        1
    );
}

#[test]
fn a_missing_draft_after_an_unknown_post_stays_pending() {
    let plan = plan();
    let api = fake(vec![]);
    lookup(&api, json!([]));
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    assert!(matches!(
        remote.observe_draft().unwrap(),
        Outcome::Pending(_)
    ));
    assert_eq!(*api.methods.borrow(), [Method::Get; 3]);
}

#[test]
fn a_changed_listing_cannot_establish_tag_absence() {
    let plan = plan();
    let api = fake(vec![]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    api.replies.borrow_mut().extend([
        Ok(Answer {
            status: 404,
            body: Value::Null,
        }),
        Ok(Answer {
            status: 200,
            body: json!([]),
        }),
        Ok(Answer {
            status: 200,
            body: json!([draft(&remote)]),
        }),
    ]);
    assert!(remote.require_no_release().is_err());
    assert!(
        api.methods
            .borrow()
            .iter()
            .all(|method| *method == Method::Get)
    );
}

#[test]
fn an_unpublished_draft_prevents_tag_cleanup() {
    let plan = plan();
    let api = fake(vec![]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    lookup(&api, json!([draft(&remote)]));
    assert!(remote.require_no_release().is_err());
    assert!(
        api.methods
            .borrow()
            .iter()
            .all(|method| *method == Method::Get)
    );
}

#[test]
fn a_proven_unstarted_post_can_be_retried_with_the_same_transaction() {
    let plan = plan();
    let api = fake(vec![]);
    lookup(&api, json!([]));
    api.replies
        .borrow_mut()
        .push_back(Err(ApiError::NotStarted));
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    assert!(matches!(
        remote.create_draft().unwrap(),
        Outcome::NotStarted(_)
    ));
    lookup(&api, json!([]));
    api.replies.borrow_mut().push_back(Ok(Answer {
        status: 201,
        body: draft(&remote),
    }));
    assert!(matches!(
        remote.create_draft().unwrap(),
        Outcome::Applied(Receipt::Draft(1))
    ));
    assert_eq!(
        api.methods
            .borrow()
            .iter()
            .filter(|method| **method == Method::Post)
            .count(),
        2
    );
}

#[test]
fn foreign_changed_or_published_drafts_are_never_adopted() {
    let plan = plan();
    let api = fake(vec![]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    for (field, replacement) in [
        ("draft", json!(false)),
        ("author", json!({"login":"other-writer"})),
        ("target_commitish", json!("ff".repeat(20))),
        ("body", json!("foreign")),
        ("published_at", json!("2026-10-30T00:00:00Z")),
        ("assets", json!([{"id":99}])),
    ] {
        let mut changed = draft(&remote);
        replace(&mut changed, field, replacement);
        assert!(remote.release_matches(&changed, None).is_err(), "{field}");
    }
    assert!(remote.release_matches(&draft(&remote), Some(2)).is_err());
}

#[test]
fn draft_cleanup_refuses_unowned_attachment_before_delete() {
    let mut plan = plan();
    plan.repositories[0].release = Some(1);
    let api = fake(vec![]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    let mut body = draft(&remote);
    replace(&mut body, "assets", json!([{"id":99}]));
    api.replies
        .borrow_mut()
        .push_back(Ok(Answer { status: 200, body }));
    assert!(remote.delete_draft(false).is_err());
    assert_eq!(*api.methods.borrow(), [Method::Get]);
}

#[test]
fn assets_need_exact_nonce_uploader_size_and_digest() {
    let plan = plan();
    let api = fake(vec![]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    let asset = json!({"id":2,"name":remote.asset_name(),"label":plan.nonce,
        "state":"uploaded","size":100,"uploader":{"login":"fixture-writer"},
        "digest":format!("sha256:{}", plan.artifact.as_ref().unwrap().sha256)});
    assert_eq!(remote.asset_matches(&asset, Some(2)).unwrap(), 2);
    for (field, replacement) in [
        ("size", json!(99)),
        ("label", json!("different")),
        ("uploader", json!({"login":"different"})),
        ("digest", json!("sha256:wrong")),
    ] {
        let mut changed = asset.clone();
        replace(&mut changed, field, replacement);
        assert!(remote.asset_matches(&changed, Some(2)).is_err());
    }
}

#[test]
fn an_owned_incomplete_upload_is_cleanupable_but_never_verified() {
    let mut plan = plan();
    plan.repositories[0].release = Some(1);
    plan.repositories[0].asset = Some(2);
    let api = fake(vec![]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    let asset = json!({"id":2,"name":remote.asset_name(),"label":plan.nonce,"state":"starter","size":0,"uploader":{"login":"fixture-writer"}});
    assert!(remote.asset_matches(&asset, Some(2)).is_err());
    assert_eq!(remote.asset_ownership(&asset, Some(2)).unwrap(), 2);
    let mut release = draft(&remote);
    replace(&mut release, "assets", json!([{"id":2}]));
    api.replies.borrow_mut().extend([
        Ok(Answer {
            status: 200,
            body: asset,
        }),
        Ok(Answer {
            status: 200,
            body: release,
        }),
        Ok(Answer {
            status: 204,
            body: Value::Null,
        }),
        Ok(Answer {
            status: 404,
            body: Value::Null,
        }),
    ]);
    assert!(matches!(
        remote.delete_asset(false).unwrap(),
        Outcome::Applied(Receipt::AssetAbsent)
    ));
    assert_eq!(
        *api.methods.borrow(),
        [Method::Get, Method::Get, Method::Delete, Method::Get]
    );
}

#[test]
fn an_already_deleted_asset_needs_no_surviving_release_to_finish_cleanup() {
    let mut plan = plan();
    plan.repositories[0].release = Some(1);
    plan.repositories[0].asset = Some(2);
    let api = fake(vec![Ok(Answer {
        status: 404,
        body: Value::Null,
    })]);
    let remote = Remote {
        api: &api,
        journal: &plan,
        index: 0,
        principal: "fixture-writer",
    };
    assert!(matches!(
        remote.delete_asset(true).unwrap(),
        Outcome::Applied(Receipt::AssetAbsent)
    ));
    assert_eq!(*api.methods.borrow(), [Method::Get]);
}
