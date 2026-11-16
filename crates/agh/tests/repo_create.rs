// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Prospective repository authorization, one-shot recovery and dry-run boundaries.

use crate::common;
use agh::config::Dirs;
use agh::github::repos::{RepoCreatePermit, RepoVisibility};
use agh::journal::{IntentStore as _, Journal, JournalRecord};
use agh::policy::{RouteRequest, RouteTarget};
use agh::write::{Effect, Ensured, Outcome};
use agh::{Github, Kind};
use agh_fake::{FakeGithub, Fault, Match, RepoSpec};

const REPO: &str = "alabsystems/aterm";
const PATH: &str = "/repos/alabsystems/aterm";
const VISIBILITIES: [(RepoVisibility, bool, &str); 2] = [
    (RepoVisibility::Public, false, "--public"),
    (RepoVisibility::Private, true, "--private"),
];

struct World {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    dirs: Dirs,
    gh: Github,
}

fn world() -> World {
    let root = common::scratch("repo-create");
    let fake = FakeGithub::start();
    common::canonical_tokens(root.path(), &fake, true, true);
    fake.add_token(&agh_fake::token("release"), "alabsystems");
    let (dirs, router) = common::file_router(root.path(), common::WRITERS);
    let gh = common::writer(router, &fake, &dirs);
    World {
        root,
        fake,
        dirs,
        gh,
    }
}

impl World {
    fn permit(&self) -> RepoCreatePermit {
        self.permit_with(RepoVisibility::Public)
    }

    fn permit_with(&self, visibility: RepoVisibility) -> RepoCreatePermit {
        self.gh
            .repos()
            .permit_create(
                &RouteRequest::write(RouteTarget::Repo(common::slug(REPO)))
                    .with_named(common::granted(&self.gh, REPO)),
                visibility,
            )
            .expect("the file grants this caller this repository")
    }

    fn create(&self, journal: &Journal) -> agh::Result<Outcome<Ensured<agh::model::RepoObject>>> {
        self.gh.repos().create(&self.permit(), journal)
    }

    fn create_with(
        &self,
        journal: &Journal,
        visibility: RepoVisibility,
    ) -> agh::Result<Outcome<Ensured<agh::model::RepoObject>>> {
        self.gh
            .repos()
            .create(&self.permit_with(visibility), journal)
    }

    fn posts(&self) -> usize {
        self.fake
            .seen()
            .iter()
            .filter(|r| r.method == "POST")
            .count()
    }

    fn cli(&self, args: &[&str]) -> (u8, agh::cli::Captured) {
        let (mut ctx, out) = common::ctx(self.root.path(), &self.fake, common::env());
        (common::run_cli(&mut ctx, args), out)
    }
}

fn done(outcome: Outcome<Ensured<agh::model::RepoObject>>, effect: Effect) -> u64 {
    done_with(outcome, effect, false)
}

fn done_with(
    outcome: Outcome<Ensured<agh::model::RepoObject>>,
    effect: Effect,
    private: bool,
) -> u64 {
    let Outcome::Done(ensured) = outcome else {
        panic!("wanted Done: {outcome:?}")
    };
    assert_eq!(ensured.effect, effect);
    assert_eq!(ensured.object.full_name, REPO);
    assert_eq!(
        ensured.object.visibility.as_deref(),
        Some(if private { "private" } else { "public" })
    );
    assert_eq!(ensured.object.private, private);
    ensured.object.id
}

#[test]
fn creates_each_visibility_with_exact_payload_and_grant_then_converges_without_posting() {
    for (visibility, private, _) in VISIBILITIES {
        let w = world();
        let journal = w.gh.journal(&common::slug(REPO)).unwrap();
        assert_eq!(w.permit_with(visibility).visibility(), visibility);
        let id = done_with(
            w.create_with(&journal, visibility).unwrap(),
            Effect::Created,
            private,
        );
        done_with(
            w.create_with(&journal, visibility).unwrap(),
            Effect::Converged,
            private,
        );
        assert_eq!(w.posts(), 1);
        let seen = w.fake.seen();
        let post = seen.iter().position(|r| r.method == "POST").unwrap();
        assert_eq!(seen[post].path_and_query, "/user/repos");
        assert_eq!(seen[post].as_login.as_deref(), Some("alabsystems"));
        let bodies = w.fake.bodies();
        let body = bodies.iter().find(|b| b.seen == post).unwrap();
        assert_eq!(
            aterm_json::from_str::<aterm_json::Value>(body.json.as_ref().unwrap()).unwrap(),
            aterm_json::json!({"name":"aterm", "private":private, "auto_init":false})
        );
        let (records, _) = journal.read().unwrap();
        let JournalRecord::Intent(intent) = &records[0].1 else {
            panic!("intent")
        };
        assert_eq!(intent.grant.as_deref(), Some("fixture-caller"));
        assert_eq!(intent.op.slug(), "repo.create");
        let digest =
            agh::model::Sha256Digest::of(body.json.as_ref().unwrap().as_bytes()).to_string();
        assert_eq!(intent.body_digest.as_deref(), Some(digest.as_str()));
        assert!(records.iter().any(|(_, r)| matches!(r,
        JournalRecord::Settled { outcome: agh::journal::Settled::Created { id: made, .. }, .. } if *made == id)));
    }
}

#[test]
fn owner_and_user_type_are_proved_even_for_file_credentials() {
    for (visibility, _, _) in VISIBILITIES {
        for wrong in ["other-user", "Organization"] {
            let w = world();
            if wrong == "Organization" {
                w.fake.fault(
                    Match::api("/user").method("GET"),
                    Fault::Status {
                        status: 200,
                        body: r#"{"id":1,"login":"alabsystems","type":"Organization"}"#.into(),
                        headers: Vec::new(),
                    },
                );
            } else {
                w.fake.add_token(&agh_fake::token("release"), wrong);
            }
            let error =
                w.gh.repos()
                    .check_create(&w.permit_with(visibility))
                    .unwrap_err();
            assert_eq!(error.kind, Kind::Refused);
            assert_eq!(w.posts(), 0);
            assert!(!w.dirs.journal().exists());
        }
    }
}

#[test]
fn ordinary_route_requires_release_literal_or_real_file_grant_and_rejects_wrong_tier() {
    for (visibility, _, _) in VISIBILITIES {
        let w = world();
        let route = RouteRequest::write(RouteTarget::Repo(common::slug(REPO)));
        assert!(w.gh.repos().permit_create(&route, visibility).is_err());
        let wrong = route
            .clone()
            .with_named(common::granted(&w.gh, REPO))
            .with_as(Some("dev".into()));
        assert!(w.gh.repos().permit_create(&wrong, visibility).is_err());
        let text_router = common::router(
            &format!(
                "{}{}",
                common::canonical_config(w.root.path()),
                common::WRITERS
            ),
            w.root.path(),
        );
        assert!(
            text_router
                .grant("fixture-caller", &common::slug(REPO))
                .is_err()
        );
        assert!(
            w.gh.repos()
                .permit_create(&RouteRequest::write(RouteTarget::Identity), visibility)
                .is_err()
        );
        assert!(w.fake.seen().is_empty());
    }
}

#[test]
fn existing_matching_visibility_is_unchanged_and_conflicting_visibility_is_never_changed() {
    for (visibility, requested_private, flag) in VISIBILITIES {
        for private in [false, true] {
            let w = world();
            w.fake.add_repo(
                REPO,
                if private {
                    RepoSpec::private().push("alabsystems")
                } else {
                    RepoSpec::public()
                },
            );
            let journal = w.gh.journal(&common::slug(REPO)).unwrap();
            if private != requested_private {
                assert_eq!(
                    w.create_with(&journal, visibility).unwrap_err().kind,
                    Kind::Conflict
                );
            } else {
                done_with(
                    w.create_with(&journal, visibility).unwrap(),
                    Effect::Unchanged,
                    private,
                );
            }
            let (exit, out) = w.cli(&["repo", "create", REPO, flag, "--dry-run"]);
            assert_eq!(
                exit,
                if private == requested_private { 0 } else { 9 },
                "{}",
                out.stderr_text()
            );
            assert_eq!(w.posts(), 0);
            assert!(journal.read().unwrap().0.is_empty());
        }
    }
}

#[test]
fn readback_requires_both_explicit_consistent_visibility_fields() {
    for (visibility, private, _) in VISIBILITIES {
        for missing_or_wrong in ["private", "visibility", "opposite-private", "internal"] {
            let w = world();
            let mut response = aterm_json::json!({
                "id":1, "full_name":REPO, "name":"aterm",
                "owner":{"id":1,"login":"alabsystems","type":"User"},
                "private":private, "visibility":if private { "private" } else { "public" },
            });
            match missing_or_wrong {
                "private" | "visibility" => {
                    response.as_object_mut().unwrap().remove(missing_or_wrong);
                }
                "opposite-private" => {
                    response
                        .as_object_mut()
                        .unwrap()
                        .insert("private".into(), (!private).into());
                }
                "internal" => {
                    response
                        .as_object_mut()
                        .unwrap()
                        .insert("visibility".into(), "internal".into());
                }
                _ => unreachable!(),
            }
            w.fake.fault(
                Match::api(PATH).method("GET"),
                Fault::Status {
                    status: 200,
                    body: response.to_string(),
                    headers: Vec::new(),
                },
            );
            let error =
                w.gh.repos()
                    .check_create(&w.permit_with(visibility))
                    .unwrap_err();
            assert_eq!(
                error.kind,
                Kind::Conflict,
                "{visibility:?}/{missing_or_wrong}"
            );
            assert_eq!(w.posts(), 0);
            assert!(!w.dirs.journal().exists());
        }
    }
}

#[test]
fn lost_applied_response_converges_and_lost_unapplied_response_never_reposts() {
    for (visibility, private, _) in VISIBILITIES {
        for lands in [false, true] {
            let w = world();
            let journal = w.gh.journal(&common::slug(REPO)).unwrap();
            w.fake.fault(
                Match::api("/user/repos").method("POST"),
                if lands {
                    Fault::LandThenClose
                } else {
                    Fault::Close
                },
            );
            let first = w.create_with(&journal, visibility).unwrap();
            if lands {
                done_with(first, Effect::Converged, private);
                done_with(
                    w.create_with(&journal, visibility).unwrap(),
                    Effect::Converged,
                    private,
                );
                assert!(journal.open().unwrap().is_empty());
            } else {
                assert!(matches!(first, Outcome::Unknown { .. }));
                assert!(matches!(
                    w.create_with(&journal, visibility).unwrap(),
                    Outcome::Unknown { .. }
                ));
                assert_eq!(journal.open().unwrap().len(), 1);
                // A later matching owner readback reconciles, without another POST.
                w.fake.add_repo(
                    REPO,
                    if private {
                        RepoSpec::private().push("alabsystems")
                    } else {
                        RepoSpec::public()
                    },
                );
                done_with(
                    w.create_with(&journal, visibility).unwrap(),
                    Effect::Converged,
                    private,
                );
            }
            assert_eq!(w.posts(), 1);
        }
    }
}

#[test]
fn changing_visibility_cannot_reconcile_or_repeat_an_unknown_creation() {
    for (visibility, private, flag) in VISIBILITIES {
        let w = world();
        let journal = w.gh.journal(&common::slug(REPO)).unwrap();
        w.fake
            .fault(Match::api("/user/repos").method("POST"), Fault::Close);
        let Outcome::Unknown { intent, .. } = w.create_with(&journal, visibility).unwrap() else {
            panic!("the original creation must remain unknown")
        };
        let before = std::fs::read(journal.path()).unwrap();
        let other = if private {
            RepoVisibility::Public
        } else {
            RepoVisibility::Private
        };
        // Even a visible object matching the new request cannot settle the old body.
        w.fake.add_repo(
            REPO,
            if private {
                RepoSpec::public()
            } else {
                RepoSpec::private().push("alabsystems")
            },
        );
        let Outcome::Unknown { intent: again, .. } = w.create_with(&journal, other).unwrap() else {
            panic!("a different visibility must not reconcile the old intent")
        };
        assert_eq!(again, intent);
        for preview_flag in [flag, if private { "--public" } else { "--private" }] {
            let (exit, _) = w.cli(&["repo", "create", REPO, preview_flag, "--dry-run"]);
            assert_eq!(exit, 13);
        }
        assert_eq!(std::fs::read(journal.path()).unwrap(), before);
        assert_eq!(journal.open().unwrap().len(), 1);
        assert_eq!(w.posts(), 1);
    }
}

#[test]
fn unknown_readback_failure_and_mismatch_keep_the_intent_unknown() {
    for private in [false, true] {
        let w = world();
        let journal = w.gh.journal(&common::slug(REPO)).unwrap();
        w.fake
            .fault(Match::api("/user/repos").method("POST"), Fault::Close);
        assert!(matches!(
            w.create(&journal).unwrap(),
            Outcome::Unknown { .. }
        ));
        if private {
            w.fake
                .add_repo(REPO, RepoSpec::private().push("alabsystems"));
        } else {
            w.fake.fault(
                Match::api(PATH).method("GET"),
                Fault::Status {
                    status: 503,
                    body: "unavailable".into(),
                    headers: Vec::new(),
                },
            );
        }
        assert!(matches!(
            w.create(&journal).unwrap(),
            Outcome::Unknown { .. }
        ));
        assert_eq!(w.posts(), 1);
        assert_eq!(journal.open().unwrap().len(), 1);
    }
}

#[test]
fn duplicate_422_is_read_back_without_repeating_post() {
    let w = world();
    let journal = w.gh.journal(&common::slug(REPO)).unwrap();
    w.fake.add_repo(REPO, RepoSpec::public());
    // Hide the initial existence check, so the POST meets GitHub's duplicate guard.
    w.fake.fault(
        Match::api(PATH).method("GET"),
        Fault::Status {
            status: 404,
            body: r#"{"message":"Not Found"}"#.into(),
            headers: Vec::new(),
        },
    );
    done(w.create(&journal).unwrap(), Effect::Unchanged);
    assert_eq!(w.posts(), 1);
    assert!(journal.open().unwrap().is_empty());
}

#[test]
fn cli_requires_exactly_one_visibility_before_any_github_calls() {
    let w = world();
    for flags in [
        vec![],
        vec!["--public", "--private"],
        vec!["--private", "--public"],
        vec!["--public", "--public"],
        vec!["--private", "--private"],
    ] {
        let mut args = vec!["repo", "create", REPO, "--dry-run"];
        args.extend(flags);
        let (exit, _) = w.cli(&args);
        assert_eq!(exit, 2);
        assert!(w.fake.seen().is_empty());
        assert!(!w.dirs.journal().exists());
    }
}

#[test]
fn both_cli_visibilities_dry_run_without_posting_journaling_or_learning_then_create() {
    for (_, private, flag) in VISIBILITIES {
        let w = world();
        let config = std::fs::read(w.dirs.config.join("agh.toml")).unwrap();
        let (exit, out) = w.cli(&["repo", "create", REPO, flag, "--dry-run"]);
        assert_eq!(exit, 0, "{}", out.stderr_text());
        let value = common::stdout_json(&out);
        assert_eq!(value["outcome"].as_str(), Some("would_send"));
        assert_eq!(value["dry_run"].as_bool(), Some(true));
        assert_eq!(w.posts(), 0);
        assert!(!w.dirs.journal().exists());
        assert_eq!(
            std::fs::read(w.dirs.config.join("agh.toml")).unwrap(),
            config
        );
        assert_eq!(
            w.fake
                .seen()
                .iter()
                .filter(|s| s.path_and_query == "/user")
                .count(),
            1
        );
        // A literal command is sufficient; the library's default target was refused above.
        let (exit, out) = w.cli(&["repo", "create", REPO, flag]);
        assert_eq!(exit, 0, "{}", out.stderr_text());
        assert_eq!(
            common::stdout_json(&out)["outcome"].as_str(),
            Some("created")
        );
        assert_eq!(w.posts(), 1);
        let body = w.fake.bodies().into_iter().find_map(|b| b.json).unwrap();
        assert_eq!(
            aterm_json::from_str::<aterm_json::Value>(&body).unwrap()["private"].as_bool(),
            Some(private)
        );
    }
}

#[test]
fn dry_run_refuses_an_open_or_torn_journal_without_healing_or_posting() {
    use std::io::Write as _;
    let w = world();
    let journal = w.gh.journal(&common::slug(REPO)).unwrap();
    w.fake
        .fault(Match::api("/user/repos").method("POST"), Fault::Close);
    assert!(matches!(
        w.create(&journal).unwrap(),
        Outcome::Unknown { .. }
    ));
    let before = std::fs::read(journal.path()).unwrap();
    let (exit, out) = w.cli(&["repo", "create", REPO, "--public", "--dry-run"]);
    assert_eq!(exit, 13, "{}", out.stderr_text());
    assert_eq!(std::fs::read(journal.path()).unwrap(), before);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal.path())
        .unwrap();
    file.write_all(b"torn").unwrap();
    let torn = std::fs::read(journal.path()).unwrap();
    let (exit, _) = w.cli(&["repo", "create", REPO, "--public", "--dry-run"]);
    assert_ne!(exit, 0);
    assert_eq!(std::fs::read(journal.path()).unwrap(), torn);
    assert_eq!(w.posts(), 1);
}

#[test]
fn closed_created_history_refuses_missing_or_replaced_repos_in_live_and_dry_run() {
    let w = world();
    let journal = w.gh.journal(&common::slug(REPO)).unwrap();
    done(w.create(&journal).unwrap(), Effect::Created);
    w.fake.rename_repo(REPO, "alabsystems/old-aterm");
    for replaced in [false, true] {
        if replaced {
            w.fake.add_repo(REPO, RepoSpec::public());
        }
        assert_eq!(w.create(&journal).unwrap_err().kind, Kind::Conflict);
        let (exit, _) = w.cli(&["repo", "create", REPO, "--public", "--dry-run"]);
        assert_eq!(exit, 9);
    }
    assert_eq!(w.posts(), 1);
}

#[test]
fn raw_api_still_cannot_create_user_or_org_repositories() {
    let w = world();
    for path in ["/user/repos", "/orgs/alabsystems/repos"] {
        let (exit, _) = w.cli(&["api", path, "-X", "POST", "-f", "name=aterm", "--dry-run"]);
        assert_eq!(exit, 2);
    }
    assert_eq!(w.posts(), 0);
}

/// Intercepts only journal behavior; network calls still hit the real loopback fake.
struct Intercept<'a> {
    journal: &'a Journal,
    race: Option<agh::journal::IntentDraft>,
    raced: std::sync::atomic::AtomicBool,
    fail_observe: bool,
}

impl agh::journal::IntentStore for Intercept<'_> {
    fn begin(&self, draft: &agh::journal::IntentDraft) -> agh::Result<agh::journal::IntentId> {
        self.journal.begin(draft)
    }
    fn settle(
        &self,
        id: &agh::journal::IntentId,
        outcome: &agh::journal::Settled,
    ) -> agh::Result<()> {
        self.journal.settle(id, outcome)
    }
    fn observe(&self, id: &agh::journal::IntentId, found: Option<u64>) -> agh::Result<()> {
        if self.fail_observe {
            return Err(agh::Error::new(
                Kind::CouldNotRun,
                "fixture observation refused",
            ));
        }
        self.journal.observe(id, found)
    }
    fn open(&self) -> agh::Result<Vec<agh::journal::Intent>> {
        self.journal.open()
    }
    fn latest(
        &self,
        op: agh::write::WriteOp,
        key: &str,
    ) -> agh::Result<Option<agh::journal::IntentView>> {
        let before = self.journal.latest(op, key)?;
        if let Some(draft) = &self.race
            && !self.raced.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let id = self.journal.begin(draft)?;
            self.journal.settle(
                &id,
                &agh::journal::Settled::Created {
                    id: 123,
                    url: String::new(),
                },
            )?;
        }
        Ok(before)
    }
}

fn draft(w: &World, slug: &str) -> agh::journal::IntentDraft {
    let permit = common::permit(
        &w.gh,
        &RouteRequest::write(RouteTarget::Repo(common::slug(REPO)))
            .with_named(common::granted(&w.gh, REPO)),
    );
    agh::journal::IntentDraft::new(
        agh::write::WriteOp::RepoCreate,
        &common::slug(slug),
        &permit,
        "fixture-fingerprint",
        agh::github::repos::CREATE_KEY,
    )
    .with_request("fixture-request".into())
}

#[test]
fn a_concurrent_completed_intent_is_not_rebased_into_a_second_post() {
    let w = world();
    let journal = w.gh.journal(&common::slug(REPO)).unwrap();
    let intercepted = Intercept {
        journal: &journal,
        race: Some(draft(&w, REPO)),
        raced: std::sync::atomic::AtomicBool::new(false),
        fail_observe: false,
    };
    assert!(w.gh.repos().create(&w.permit(), &intercepted).is_err());
    assert_eq!(w.posts(), 0);
}

#[test]
fn failed_observation_keeps_the_same_unknown_intent_without_another_post() {
    let w = world();
    let journal = w.gh.journal(&common::slug(REPO)).unwrap();
    let intercepted = Intercept {
        journal: &journal,
        race: None,
        raced: std::sync::atomic::AtomicBool::new(false),
        fail_observe: true,
    };
    w.fake.fault(
        Match::api("/user/repos").method("POST"),
        Fault::LandThenClose,
    );
    for _ in 0..2 {
        let Outcome::Unknown { error, .. } =
            w.gh.repos().create(&w.permit(), &intercepted).unwrap()
        else {
            panic!("the observation did not settle")
        };
        assert_eq!(error.side_effect, agh::SideEffect::Unknown);
    }
    assert_eq!(w.posts(), 1);
    assert_eq!(journal.open().unwrap().len(), 1);
}

#[test]
fn a_foreign_intent_store_is_refused_before_reads_or_observations() {
    let w = world();
    let journal = w.gh.journal(&common::slug("other/aterm")).unwrap();
    let id = journal.begin(&draft(&w, "other/aterm")).unwrap();
    journal
        .settle(
            &id,
            &agh::journal::Settled::Unknown {
                why: "fixture".into(),
            },
        )
        .unwrap();
    assert_eq!(
        w.gh.repos().create(&w.permit(), &journal).unwrap_err().kind,
        Kind::Usage
    );
    assert!(w.fake.seen().is_empty());
    assert_eq!(journal.open().unwrap().len(), 1);
}

#[test]
fn timeout_leaves_one_unknown_intent_and_retry_does_not_send_again() {
    let w = world();
    w.fake.fault(
        Match::api("/user/repos").method("POST"),
        Fault::Hang(std::time::Duration::from_secs(3)),
    );
    let (exit, out) = w.cli(&["repo", "create", REPO, "--public", "--timeout", "1s"]);
    assert_eq!(exit, 13, "{}", out.stderr_text());
    let (exit, _) = w.cli(&["repo", "create", REPO, "--public"]);
    assert_eq!(exit, 13);
    assert_eq!(w.posts(), 1);
}

#[test]
fn dry_run_and_loopback_recording_never_learn_unrecorded_vault_tokens() {
    use agh::identity::vault;
    for (_, _, flag) in VISIBILITIES {
        for record in [false, true] {
            let w = world();
            let explicit = common::canonical_config(w.root.path())
                .replace("profiles.dev", "profiles.explicit-dev")
                .replace("profiles.release", "profiles.explicit-release")
                .replace("read = [\"dev\"", "read = [\"explicit-dev\"")
                .replace("write = \"dev\"", "write = \"explicit-dev\"")
                .replace("write = \"release\"", "write = \"explicit-release\"");
            common::write_config(
                &w.dirs,
                &format!("vault = true\n{explicit}{}", common::WRITERS),
            );
            for (slot, seed, login) in [
                (vault::Slot::Dev, "unlearned-dev", "new-dev"),
                (vault::Slot::Release, "unlearned-release", "new-release"),
            ] {
                let token = agh_fake::token(seed);
                common::vault_entry(&w.dirs.home, slot.entry(), &token);
                w.fake.add_token(&token, login);
            }
            let tape = w.root.path().join("dry.atape");
            let mut args = vec!["repo", "create", REPO, flag, "--dry-run"];
            if record {
                args.extend(["--record", tape.to_str().unwrap()]);
            }
            let (exit, out) = w.cli(&args);
            assert_eq!(exit, 0, "{}", out.stderr_text());
            assert_eq!(w.posts(), 0);
            assert!(!w.dirs.config.join(vault::RECORD_FILE).exists());
            assert!(!w.dirs.journal().exists());
            assert_eq!(
                w.fake
                    .seen()
                    .iter()
                    .filter(|s| s.path_and_query == "/user")
                    .count(),
                1
            );
        }
    }
}
