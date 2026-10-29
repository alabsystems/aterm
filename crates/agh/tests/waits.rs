// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh release wait` tests on simulated time.

use std::time::Duration;

use agh::cli::{Captured, Ctx};
use agh::error::Kind;
use agh::model::Sha256Digest;
use agh::wait::{Backoff, ReleaseCond};
use agh_fake::{AssetSpec, Change, FakeGithub, Fault, Match, ReleaseSpec, RepoSpec};
use aterm_json::Value;

use crate::common;

const REPO: &str = "alabsystems/aterm";
const TAGS: &str = "/repos/alabsystems/aterm/releases/tags/";
const CREATED: &str = "2026-10-01T00:00:00Z";

fn fake() -> FakeGithub {
    let fake = FakeGithub::start();
    fake.add_repo(REPO, RepoSpec::public());
    fake
}

fn context(root: &std::path::Path, fake: &FakeGithub) -> (Ctx, Captured) {
    common::ctx(root, fake, common::env())
}

fn wait(ctx: &mut Ctx, args: &[&str]) -> u8 {
    let mut line = vec!["release", "wait", REPO];
    line.extend_from_slice(args);
    common::run_cli(ctx, &line)
}

fn error_lines(captured: &Captured) -> Vec<String> {
    captured
        .stderr_text()
        .lines()
        .filter(|line| !line.starts_with("agh: waiting "))
        .map(str::to_string)
        .collect()
}

fn error_json(captured: &Captured) -> Value {
    common::error_json(&error_lines(captured).join("\n"))
}

fn tag_reads(fake: &FakeGithub) -> Vec<agh_fake::Seen> {
    fake.seen()
        .into_iter()
        .filter(|s| s.path_and_query.starts_with(TAGS))
        .collect()
}

fn release_etag(root: &std::path::Path, fake: &FakeGithub) -> String {
    let (mut ctx, _captured) = context(root, fake);
    let gh = ctx.github().expect("the client");
    let slug = agh::slug::Slug::parse(REPO).expect("a slug");
    let call = agh::client::Call::get(
        format!("{TAGS}v1.0.0"),
        agh::github::How::read().route(agh::policy::RouteTarget::Repo(slug)),
    );
    let answered = gh.call(&call).expect("the release");
    assert_eq!(answered.status, 200);
    answered.etag.expect("an ETag")
}

#[test]
fn a_draft_published_mid_wait_holds() {
    let root = common::scratch("wait-held");
    let fake = fake();
    let id = fake.add_release(REPO, ReleaseSpec::draft("v1.0.0", CREATED));
    fake.after(Match::api(TAGS), 3, Change::Publish { release_id: id });
    let (mut ctx, captured) = context(root.path(), &fake);
    assert_eq!(
        wait(&mut ctx, &["v1.0.0", "published"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let state = common::stdout_json(&captured);
    let condition = &state["conditions"][0];
    assert_eq!(condition["cond"], Value::from("published"));
    assert_eq!(condition["holds"], Value::from(true));
    assert_eq!(condition["observed"]["id"], Value::from(id));
    assert_eq!(condition["observed"]["draft"], Value::from(false));
    assert_eq!(tag_reads(&fake).len(), 4);
    assert!(state["elapsed_s"].as_u64().is_some());
    assert_eq!(
        ctx.fx.slept(),
        [
            Duration::from_secs(2),
            Duration::from_secs(3),
            Duration::from_millis(4500)
        ]
    );
}

#[test]
fn a_wait_revalidates_with_the_last_etag() {
    let root = common::scratch("wait-etag");
    let fake = fake();
    let id = fake.add_release(REPO, ReleaseSpec::published("v1.0.0", CREATED));
    fake.add_asset(id, AssetSpec::uploaded("a.tar.gz", b"one"));
    let etag = release_etag(root.path(), &fake);
    let before = tag_reads(&fake).len();
    fake.after(
        Match::api(TAGS),
        3,
        Change::AddAsset {
            release_id: id,
            spec: AssetSpec::uploaded("b.tar.gz", b"two"),
        },
    );
    let (mut ctx, captured) = context(root.path(), &fake);
    assert_eq!(
        wait(&mut ctx, &["v1.0.0", "assets=2"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let reads = tag_reads(&fake).split_off(before);
    assert_eq!(reads.len(), 4);
    assert!(reads[0].header("if-none-match").is_none());
    for read in &reads[1..] {
        assert_eq!(
            read.header("if-none-match"),
            Some(etag.as_str()),
            "{read:?}"
        );
    }
    let state = common::stdout_json(&captured);
    assert_eq!(state["conditions"][0]["observed"], Value::from(2u64));
    assert_eq!(
        ctx.fx.slept(),
        [
            Duration::from_secs(2),
            Duration::from_secs(3),
            Duration::from_millis(4500)
        ]
    );
}

#[test]
fn a_304_is_an_observation_of_the_last_answer() {
    let root = common::scratch("wait-304");
    let fake = fake();
    let id = fake.add_release(REPO, ReleaseSpec::published("v1.0.0", CREATED));
    fake.add_asset(id, AssetSpec::uploaded("a.tar.gz", b"one"));
    let etag = release_etag(root.path(), &fake);
    let before = tag_reads(&fake).len();
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v1.0.0", "assets=2", "--timeout", "120s"]);
    assert_eq!(exit, 8, "{}", captured.stderr_text());
    let state = common::stdout_json(&captured);
    assert_eq!(state["conditions"][0]["observed"], Value::from(1u64));
    assert_eq!(state["elapsed_s"], Value::from(120u64));
    let reads = tag_reads(&fake).split_off(before);
    assert!(reads.len() > 2, "{}", reads.len());
    for read in &reads[1..] {
        assert_eq!(
            read.header("if-none-match"),
            Some(etag.as_str()),
            "{read:?}"
        );
    }
}

#[test]
fn a_dot_segment_tag_or_asset_name_is_usage_before_any_request() {
    let root = common::scratch("wait-dots");
    let fake = fake();
    for args in [
        ["..", "anon-visible=SHA256SUMS"],
        [".", "exists"],
        ["v1.0.0", "anon-visible=.."],
        ["v1.0.0", "asset=a,.."],
    ] {
        let (mut ctx, captured) = context(root.path(), &fake);
        assert_eq!(
            wait(&mut ctx, &args),
            2,
            "{args:?}: {}",
            captured.stderr_text()
        );
        assert_eq!(
            error_json(&captured)["kind"],
            Value::from("usage"),
            "{args:?}"
        );
    }
    assert!(fake.seen().is_empty());
}

#[test]
fn a_heartbeat_redacts_before_it_cuts() {
    let root = common::scratch("wait-redact-cut");
    let fake = fake();
    let secret = "learned-opaque-value-for-the-cut-test";
    let tag = format!("{}{secret}", "x".repeat(105));
    fake.add_release(REPO, ReleaseSpec::published(&tag, CREATED));
    let (mut ctx, captured) = context(root.path(), &fake);
    ctx.redactor.learn(secret.as_bytes());
    let exit = wait(&mut ctx, &[&tag, "prerelease", "--timeout", "70s"]);
    assert_eq!(exit, 8, "{}", captured.stderr_text());
    let stderr = captured.stderr_text();
    let beat = stderr
        .lines()
        .find(|line| line.starts_with("agh: waiting "))
        .expect("a heartbeat");
    assert!(!beat.contains(&secret[..8]), "{beat}");
    assert!(beat.contains("[redacted]"), "{beat}");
}

#[test]
fn a_timeout_prints_the_state_and_exits_8() {
    let root = common::scratch("wait-timeout");
    let fake = fake();
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v9.9.9", "published", "--timeout", "10s"]);
    assert_eq!(exit, 8);
    let state = common::stdout_json(&captured);
    assert_eq!(state["conditions"][0]["holds"], Value::from(false));
    assert!(state["conditions"][0]["observed"].is_null());
    assert_eq!(state["elapsed_s"], Value::from(10u64));
    let stderr = captured.stderr_text();
    assert_eq!(stderr.lines().count(), 2, "{stderr}");
    assert_eq!(error_json(&captured)["kind"], Value::from("pending"));
    assert_eq!(
        ctx.fx.slept().iter().sum::<Duration>(),
        Duration::from_secs(10)
    );
    assert_eq!(ctx.fx.slept()[0], Backoff::STANDARD.start);
}

#[test]
fn a_failing_check_keeps_its_last_observation_in_the_state() {
    let root = common::scratch("wait-unknown");
    let fake = fake();
    fake.add_release(REPO, ReleaseSpec::published("v1.0.0", CREATED));
    let status = |status: u16| Fault::Status {
        status,
        body: String::new(),
        headers: vec![],
    };
    fake.fault(Match::web("/"), status(404));
    fake.fault_always(Match::web("/"), status(500));
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(
        &mut ctx,
        &["v1.0.0", "exists", "anon-visible=a.bin", "--timeout", "10s"],
    );
    assert_eq!(exit, 8, "{}", captured.stderr_text());
    let state = common::stdout_json(&captured);
    assert_eq!(state["conditions"][0]["holds"], Value::from(true));
    let anon = &state["conditions"][1];
    assert_eq!(anon["holds"], Value::from(false), "{anon}");
    assert_eq!(anon["observed"], Value::from("absent"), "{anon}");
}

#[test]
fn a_wait_with_no_observation_for_60s_is_blind() {
    let root = common::scratch("wait-blind");
    let fake = fake();
    fake.fault_always(
        Match::api(TAGS),
        Fault::Status {
            status: 500,
            body: String::new(),
            headers: vec![],
        },
    );
    let (mut ctx, captured) = context(root.path(), &fake);
    assert_eq!(wait(&mut ctx, &["v1.0.0", "exists"]), 3);
    let error = error_json(&captured);
    assert_eq!(error["kind"], Value::from("could_not_run"), "{error}");
    assert!(captured.stdout().is_empty());
    let slept: Duration = ctx.fx.slept().iter().sum();
    assert!(slept >= Duration::from_secs(60), "{slept:?}");
}

#[test]
fn a_heartbeat_goes_to_stderr_each_60s() {
    let root = common::scratch("wait-heartbeat");
    let fake = fake();
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v9.9.9", "exists", "--timeout", "150s"]);
    assert_eq!(exit, 8);
    let stderr = captured.stderr_text();
    let beats: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("agh: waiting "))
        .collect();
    assert_eq!(beats.len(), 2, "{stderr}");
    for beat in &beats {
        assert!(beat.contains(" verb=release.wait "), "{beat}");
        assert!(beat.contains(" repo=alabsystems/aterm "), "{beat}");
        assert!(beat.contains(" for=exists "), "{beat}");
        assert!(beat.contains(" last=\"exists:null\""), "{beat}");
    }
    assert_eq!(error_lines(&captured).len(), 2, "{stderr}");
}

#[test]
fn a_repository_that_turns_invisible_ends_the_wait() {
    let root = common::scratch("wait-hide");
    let fake = fake();
    fake.after(
        Match::api(TAGS),
        2,
        Change::Hide {
            slug: REPO.to_string(),
        },
    );
    let (mut ctx, captured) = context(root.path(), &fake);
    assert_eq!(
        wait(&mut ctx, &["v1.0.0", "published"]),
        7,
        "{}",
        captured.stderr_text()
    );
    assert_eq!(error_json(&captured)["kind"], Value::from("not_visible"));
}

#[test]
fn gone_holds_once_the_release_is_deleted() {
    let root = common::scratch("wait-gone");
    let fake = fake();
    let id = fake.add_release(REPO, ReleaseSpec::published("v1.0.0", CREATED));
    fake.after(
        Match::api(TAGS),
        2,
        Change::DeleteRelease { release_id: id },
    );
    let (mut ctx, captured) = context(root.path(), &fake);
    assert_eq!(
        wait(&mut ctx, &["v1.0.0", "gone"]),
        0,
        "{}",
        captured.stderr_text()
    );
    let state = common::stdout_json(&captured);
    assert!(state["conditions"][0]["observed"].is_null());
}

#[test]
fn asset_and_digest_conditions_hold_together() {
    let root = common::scratch("wait-digest");
    let fake = fake();
    let id = fake.add_release(REPO, ReleaseSpec::published("v1.0.0", CREATED));
    fake.add_asset(id, AssetSpec::uploaded("SHA256SUMS", b"sums"));
    let digest = Sha256Digest::from_bytes(aterm_digest::Sha256::digest(b"sums"));
    let digest_cond = format!("digest=SHA256SUMS:{digest}");
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(
        &mut ctx,
        &[
            "v1.0.0",
            "exists",
            "asset=SHA256SUMS",
            &digest_cond,
            "--timeout",
            "30s",
        ],
    );
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let state = common::stdout_json(&captured);
    let conds: Vec<&str> = state["conditions"]
        .as_array()
        .expect("conditions")
        .iter()
        .filter_map(|c| c["cond"].as_str())
        .collect();
    assert_eq!(conds, ["exists", "asset=SHA256SUMS", digest_cond.as_str()]);
    assert_eq!(
        state["conditions"][1]["observed"],
        aterm_json::json!([{"name": "SHA256SUMS", "state": "uploaded"}])
    );
}

/// The canonical config with both tokens, and `alabsystems/aterm` pushable by the release
/// login: a WriterRead sees drafts.
#[cfg(unix)] // File credentials are unix-only (plan §1, rule 12).
fn writer_context(root: &std::path::Path) -> (FakeGithub, Ctx, Captured) {
    let fake = FakeGithub::start();
    fake.add_repo(REPO, RepoSpec::public().push("fixture-release-bot"));
    common::canonical_tokens(root, &fake, true, true);
    let (ctx, captured) = context(root, &fake);
    let dirs = ctx.dirs.clone().expect("dirs");
    common::write_config(&dirs, &common::canonical_config(root));
    (fake, ctx, captured)
}

#[cfg(unix)] // File credentials are unix-only (plan §1, rule 12).
#[test]
fn include_drafts_sees_a_draft_and_two_drafts_conflict() {
    let root = common::scratch("wait-drafts");
    let (fake, mut ctx, captured) = writer_context(root.path());
    let id = fake.add_release(REPO, ReleaseSpec::draft("v1.0.0", CREATED));
    let exit = wait(&mut ctx, &["v1.0.0", "exists", "--include-drafts"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let state = common::stdout_json(&captured);
    assert_eq!(state["conditions"][0]["observed"]["id"], Value::from(id));
    assert_eq!(
        state["conditions"][0]["observed"]["draft"],
        Value::from(true)
    );

    fake.add_release(REPO, ReleaseSpec::draft("v1.0.0", CREATED));
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v1.0.0", "exists", "--include-drafts"]);
    assert_eq!(exit, 9, "{}", captured.stderr_text());
    assert_eq!(error_json(&captured)["sub"], Value::from("ambiguous_tag"));
}

#[cfg(unix)] // File credentials are unix-only (plan §1, rule 12).
#[test]
fn gone_with_drafts_holds_while_only_drafts_have_the_tag() {
    let root = common::scratch("wait-gone-drafts");
    let (fake, mut ctx, captured) = writer_context(root.path());
    let id = fake.add_release(REPO, ReleaseSpec::draft("v1.0.0", CREATED));
    let exit = wait(&mut ctx, &["v1.0.0", "gone", "--include-drafts"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    let state = common::stdout_json(&captured);
    assert_eq!(state["conditions"][0]["observed"]["id"], Value::from(id));

    fake.add_release(REPO, ReleaseSpec::draft("v1.0.0", CREATED));
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v1.0.0", "gone", "--include-drafts"]);
    assert_eq!(exit, 0, "{}", captured.stderr_text());

    fake.add_release(REPO, ReleaseSpec::published("v1.0.0", CREATED));
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(
        &mut ctx,
        &["v1.0.0", "gone", "--include-drafts", "--timeout", "10s"],
    );
    assert_eq!(exit, 8, "{}", captured.stderr_text());
    let state = common::stdout_json(&captured);
    assert_eq!(
        state["conditions"][0]["observed"]["draft"],
        Value::from(false)
    );
}

#[test]
fn a_bad_condition_is_usage_before_any_request() {
    let root = common::scratch("wait-bad");
    let fake = fake();
    let (mut ctx, captured) = context(root.path(), &fake);
    assert_eq!(wait(&mut ctx, &["v1.0.0", "shipped"]), 2);
    let error = error_json(&captured);
    assert_eq!(error["kind"], Value::from("usage"));
    assert!(fake.seen().is_empty());
}

#[test]
fn every_condition_form_round_trips() {
    let digest = format!("sha256:{}", "ab".repeat(32));
    let forms = [
        "exists".to_string(),
        "published".to_string(),
        "prerelease".to_string(),
        "latest".to_string(),
        "not-latest".to_string(),
        "assets=0".to_string(),
        "assets=12".to_string(),
        "asset=SHA256SUMS".to_string(),
        "asset=a.tar.gz,b.zip".to_string(),
        format!("digest=SHA256SUMS:{digest}"),
        "anon-visible=SHA256SUMS".to_string(),
        "gone".to_string(),
    ];
    for form in &forms {
        let cond = ReleaseCond::parse(form).expect(form);
        assert_eq!(&cond.slug(), form);
        assert_eq!(ReleaseCond::parse(&cond.slug()).expect(form), cond);
    }
    assert_eq!(
        ReleaseCond::parse("assets=3").expect("assets"),
        ReleaseCond::Assets(3)
    );
    assert_eq!(
        ReleaseCond::parse("asset=a,b").expect("asset"),
        ReleaseCond::Asset(vec!["a".into(), "b".into()])
    );
    for bad in [
        "shipped",
        "assets=",
        "assets=-1",
        "assets=+1",
        "assets=two",
        "asset=",
        "asset=a,,b",
        "digest=SHA256SUMS",
        "digest=:sha256:00",
        "digest=X:sha256:zz",
        "anon-visible=",
        "anon-visible=..",
        "anon-visible=a/../b",
        "anon-visible=/lead",
        "anon-visible=a//b",
        "asset=.",
        "asset=a/../b",
        "asset=trail/",
        "latest=1",
    ] {
        let error = ReleaseCond::parse(bad).expect_err(bad);
        assert_eq!(error.kind, Kind::Usage, "{bad}");
        for form in [
            "exists",
            "assets=N",
            "digest=NAME:sha256:HEX",
            "anon-visible=NAME",
            "gone",
        ] {
            assert!(error.next.contains(form), "{bad}: {}", error.next);
        }
    }
}

#[test]
fn the_backoff_grows_resets_and_stays_in_bounds() {
    let b = Backoff::STANDARD;
    let ms = Duration::from_millis;
    for (current, changed, next) in [
        (ms(2000), false, ms(3000)),
        (ms(3000), false, ms(4500)),
        (ms(4500), false, ms(6750)),
        (ms(15187), false, ms(22780)),
        (ms(22780), false, ms(30000)),
        (ms(30000), false, ms(30000)),
        (ms(30000), true, ms(2000)),
        (ms(100), false, ms(1000)),
        (ms(500), true, ms(2000)),
    ] {
        assert_eq!(b.next(current, changed), next, "{current:?} {changed}");
    }
}

#[test]
fn an_empty_tag_or_tag_part_is_usage_before_any_request() {
    let root = common::scratch("wait-empty-tag");
    let fake = fake();
    fake.add_release(REPO, ReleaseSpec::published("v1.0.0", CREATED));
    for args in [["", "gone"], ["v1/", "exists"], ["a//b", "published"]] {
        let (mut ctx, captured) = context(root.path(), &fake);
        assert_eq!(
            wait(&mut ctx, &args),
            2,
            "{args:?}: {}",
            captured.stderr_text()
        );
        assert_eq!(
            error_json(&captured)["kind"],
            Value::from("usage"),
            "{args:?}"
        );
    }
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn a_heartbeat_quotes_its_fields_and_a_control_byte_is_no_asset_name() {
    let root = common::scratch("wait-fields");
    let fake = fake();
    let (mut ctx, captured) = context(root.path(), &fake);
    let forged = "asset=a\nagh: ok exit=0 verb=forged";
    assert_eq!(wait(&mut ctx, &["v1.0.0", forged, "--timeout", "70s"]), 2);
    assert!(
        captured
            .stderr_text()
            .lines()
            .all(|line| !line.starts_with("agh: ok ")),
        "{}",
        captured.stderr_text()
    );
    assert!(fake.seen().is_empty());

    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v1.0.0", "asset=a b", "--timeout", "70s"]);
    assert_eq!(exit, 8, "{}", captured.stderr_text());
    let stderr = captured.stderr_text();
    let beat = stderr
        .lines()
        .find(|line| line.starts_with("agh: waiting "))
        .expect("a heartbeat");
    assert!(beat.contains(" for=\"asset=a b\" "), "{beat}");
}

#[test]
fn an_anonymous_wait_on_a_spent_budget_goes_blind_with_the_rate_limit() {
    let root = common::scratch("wait-rate");
    let fake = fake();
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v1.0.0", "exists", "published"]);
    let stderr = captured.stderr_text();
    assert_eq!(exit, 3, "{stderr}");
    let error = error_json(&captured);
    assert_eq!(error["kind"], Value::from("could_not_run"), "{error}");
    assert!(
        error["why"].as_str().is_some_and(|why| why.contains("60s")),
        "{error}"
    );
    assert!(error["reset"].as_u64().is_some(), "{error}");
    assert!(captured.stdout().is_empty());
}

struct Never {
    observed: String,
}

impl agh::wait::Condition for Never {
    fn describe(&self) -> String {
        "never".to_string()
    }

    fn check(&mut self, _gh: &agh::client::Github) -> agh::error::Result<agh::wait::Verdict> {
        Ok(agh::wait::Verdict::NotYet(Value::from(
            self.observed.as_str(),
        )))
    }
}

#[test]
fn the_poller_redacts_then_cuts_a_heartbeats_last() {
    let root = common::scratch("wait-poller-last");
    let fake = fake();
    let (mut ctx, _captured) = context(root.path(), &fake);
    let secret = "learned-opaque-value-for-the-poller-test";
    ctx.redactor.learn(secret.as_bytes());
    let gh = ctx.github().expect("the client");
    let mut conditions: Vec<Box<dyn agh::wait::Condition>> = vec![Box::new(Never {
        observed: format!("{}{secret}{}", "x".repeat(150), "y".repeat(300)),
    })];
    let poller = agh::wait::Poller {
        fx: gh.fx().as_ref(),
        backoff: Backoff::STANDARD,
        timeout: Duration::from_secs(70),
        heartbeat: Duration::from_secs(60),
        blind_limit: Duration::from_secs(60),
    };
    let mut lasts = Vec::new();
    let end = poller.run(&gh, &mut conditions, &mut |event| {
        if let agh::wait::WaitEvent::Heartbeat { last, .. } = event {
            lasts.push(last.to_string());
        }
    });
    assert!(matches!(end, agh::wait::WaitEnd::TimedOut { .. }));
    assert_eq!(lasts.len(), 1);
    assert_eq!(lasts[0].chars().count(), 200, "{}", lasts[0]);
    assert!(!lasts[0].contains(&secret[..8]), "{}", lasts[0]);
    assert!(lasts[0].contains("[redacted]"), "{}", lasts[0]);
}

struct HeldThenFailing {
    checks: usize,
}

impl agh::wait::Condition for HeldThenFailing {
    fn describe(&self) -> String {
        "exists".to_string()
    }

    fn check(&mut self, _gh: &agh::client::Github) -> agh::error::Result<agh::wait::Verdict> {
        self.checks += 1;
        if self.checks == 1 {
            return Ok(agh::wait::Verdict::Holds(
                aterm_json::json!({"assets": 1, "draft": false, "id": 7}),
            ));
        }
        Err(agh::error::Error::could_not_run("GitHub answered 500"))
    }
}

#[test]
fn a_condition_failing_until_the_timeout_keeps_its_last_observation() {
    let root = common::scratch("wait-poller-failing");
    let fake = fake();
    let (mut ctx, _captured) = context(root.path(), &fake);
    let gh = ctx.github().expect("the client");
    let mut conditions: Vec<Box<dyn agh::wait::Condition>> = vec![
        Box::new(HeldThenFailing { checks: 0 }),
        Box::new(Never {
            observed: "1".to_string(),
        }),
    ];
    let poller = agh::wait::Poller {
        fx: gh.fx().as_ref(),
        backoff: Backoff::STANDARD,
        timeout: Duration::from_secs(10),
        heartbeat: Duration::from_secs(60),
        blind_limit: Duration::from_secs(60),
    };
    let end = poller.run(&gh, &mut conditions, &mut |_| {});
    let agh::wait::WaitEnd::TimedOut { last } = end else {
        panic!("the wait times out");
    };
    let exists = &last["conditions"][0];
    assert_eq!(exists["holds"], Value::from(true), "{last}");
    assert_eq!(exists["observed"]["id"], Value::from(7u64), "{last}");
    assert_eq!(
        last["conditions"][1]["observed"],
        Value::from("1"),
        "{last}"
    );
}

#[test]
fn a_rate_limit_past_the_blind_limit_is_blind_on_the_backoff() {
    let root = common::scratch("wait-rate-reset");
    let fake = fake();
    fake.add_release(REPO, ReleaseSpec::published("v1.0.0", CREATED));
    let reset = 1_760_000_120u64.to_string();
    // Standing, as GitHub refuses every request until the reset: Unix's rate
    // book refuses the later rounds before they are sent, Windows (no book)
    // sends them.
    fake.fault_always(
        Match::api(TAGS),
        Fault::Status {
            status: 403,
            body: String::new(),
            headers: vec![
                ("x-ratelimit-limit".to_string(), "60".to_string()),
                ("x-ratelimit-remaining".to_string(), "0".to_string()),
                ("x-ratelimit-used".to_string(), "60".to_string()),
                ("x-ratelimit-resource".to_string(), "core".to_string()),
                ("x-ratelimit-reset".to_string(), reset),
            ],
        },
    );
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v1.0.0", "exists"]);
    assert_eq!(exit, 3, "{}", captured.stderr_text());
    let error = error_json(&captured);
    assert_eq!(error["reset"], Value::from(1_760_000_120u64), "{error}");
    let slept = ctx.fx.slept();
    assert_eq!(
        slept[..3],
        [
            Duration::from_secs(3),
            Duration::from_millis(4500),
            Duration::from_millis(6750)
        ]
    );
    let total: Duration = slept.iter().sum();
    assert!(total >= Duration::from_secs(60), "{slept:?}");
    assert!(total < Duration::from_secs(120), "{slept:?}");
}

#[test]
fn a_wait_refuses_no_probe_before_any_request() {
    let root = common::scratch("wait-no-probe");
    let fake = fake();
    let (mut ctx, captured) = context(root.path(), &fake);
    let exit = wait(&mut ctx, &["v1.0.0", "exists", "--no-probe"]);
    assert_eq!(exit, 2, "{}", captured.stderr_text());
    let error = error_json(&captured);
    assert_eq!(error["kind"], Value::from("usage"), "{error}");
    assert!(
        error["next"]
            .as_str()
            .is_some_and(|next| next.contains("release wait")),
        "{error}"
    );
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

#[test]
fn a_library_anon_visible_wait_on_a_bad_tag_is_usage_before_any_request() {
    let root = common::scratch("wait-anon-bad-tag");
    let fake = fake();
    let (mut ctx, _captured) = context(root.path(), &fake);
    let gh = ctx.github().expect("the client");
    let slug = agh::slug::Slug::parse(REPO).expect("a slug");
    for tag in ["", ".."] {
        let mut condition = agh::wait::release_condition(
            &slug,
            tag,
            ReleaseCond::AnonVisible("a.bin".to_string()),
            agh::github::How::read(),
        );
        let error = condition
            .check(&gh)
            .err()
            .unwrap_or_else(|| panic!("tag {tag:?} is refused"));
        assert_eq!(error.kind, Kind::Usage, "{tag:?}: {error:?}");
    }
    for cond in [
        ReleaseCond::AnonVisible("..".to_string()),
        ReleaseCond::AnonVisible("a/../b".to_string()),
        ReleaseCond::AnonVisible("/lead".to_string()),
        ReleaseCond::Asset(vec!["a.bin".to_string(), "a//b".to_string()]),
    ] {
        let label = cond.slug();
        let mut condition =
            agh::wait::release_condition(&slug, "v1.0.0", cond, agh::github::How::read());
        let error = condition
            .check(&gh)
            .err()
            .unwrap_or_else(|| panic!("condition {label:?} is refused"));
        assert_eq!(error.kind, Kind::Usage, "{label:?}: {error:?}");
    }
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}
