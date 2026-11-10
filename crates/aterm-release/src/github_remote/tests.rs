// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::{GitCli, GitRunner, object_git};

#[test]
fn lane_record_reads_one_file_at_the_observed_journal_without_importing_history() {
    let fake = agh_fake::FakeGithub::start();
    let slug = Slug::parse("fixture/app").unwrap();
    fake.add_repo("fixture/app", agh_fake::RepoSpec::public());
    let journal = "a".repeat(40);
    let tree = "b".repeat(40);
    let path = format!("verdicts/{tree}.toml");
    fake.put_file("fixture/app", &journal, &path, b"state = 'red'");
    fake.put_file("fixture/app", "main", &path, b"state = 'pass'");
    let ports = fake.ports();
    let env = Arc::new(agh::identity::MapEnv::default());
    let client = GithubBuilder::new(Router::empty())
        .endpoints(agh::wire::Endpoints::loopback(
            ports.api,
            ports.uploads,
            ports.web,
            ports.cdn,
        ))
        .transport(Arc::new(agh::wire::http::HttpTransport::direct(
            env.clone(),
        )))
        .env(env)
        .fx(Arc::new(agh::Fx::simulated(1_790_000_000)))
        .retry(agh::retry::RetryPolicy::NONE)
        .build();
    assert_eq!(
        lane_record_with(&client, &slug, &journal, &tree)
            .unwrap()
            .unwrap(),
        b"state = 'red'"
    );
    assert_eq!(
        lane_record_with(&client, &slug, &journal, &"c".repeat(40)).unwrap(),
        None
    );
    assert!(lane_record_with(&client, &slug, "main", &tree).is_err());
    assert!(lane_record_with(&client, &slug, &journal, "../other").is_err());
    // A private, invisible parent is not evidence that a tree is unjudged.
    fake.add_repo("hidden/app", agh_fake::RepoSpec::private());
    assert!(
        lane_record_with(
            &client,
            &Slug::parse("hidden/app").unwrap(),
            &journal,
            &tree
        )
        .is_err()
    );
    fake.assert_clean();
}

#[test]
fn supersession_reads_private_staging_with_its_routed_identity() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let fake = agh_fake::FakeGithub::start();
    let token = agh_fake::token("supersession-dev");
    fake.add_token(&token, "fixture-dev-bot");
    fake.add_repo(
        "fixture-dev/staging",
        agh_fake::RepoSpec::private().pull("fixture-dev-bot"),
    );
    fake.put_file(
        "fixture-dev/staging",
        "main",
        "Cargo.toml",
        b"staged version",
    );
    let token_path = root.path().join("dev-token");
    std::fs::write(&token_path, token).unwrap();
    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = format!(
        "[profiles.dev]\ntoken_file = {token_path:?}\nlogin = \"fixture-dev-bot\"\ntier = \"dev\"\n[[owners]]\nname = \"fixture-dev\"\ntier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n"
    );
    let parsed = agh::config::parse(&config).unwrap();
    let validated = agh::config::validate(&parsed, root.path(), &agh::config::file_id).unwrap();
    let client = |router| {
        let ports = fake.ports();
        let env = Arc::new(agh::identity::MapEnv::default());
        GithubBuilder::new(router)
            .endpoints(agh::wire::Endpoints::loopback(
                ports.api,
                ports.uploads,
                ports.web,
                ports.cdn,
            ))
            .transport(Arc::new(agh::wire::http::HttpTransport::direct(
                env.clone(),
            )))
            .env(env)
            .fx(Arc::new(agh::Fx::simulated(1_790_000_000)))
            .retry(agh::retry::RetryPolicy::NONE)
            .build()
    };
    let routed = client(Router::from_validated(validated));
    assert_eq!(
        repository_file_with(&routed, "fixture-dev/staging", "main", "Cargo.toml").unwrap(),
        b"staged version"
    );
    // Negative control: an unconfigured identity cannot establish private staging state.
    let anonymous = client(Router::empty());
    assert!(repository_file_with(&anonymous, "fixture-dev/staging", "main", "Cargo.toml").is_err());
    fake.assert_clean();
}

fn git(repo: &Path, args: &[&str]) -> String {
    let mut cmd = object_git(repo).unwrap();
    cmd.args(args)
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
    let out = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Explicit local-bare fixture route in agh itself, never a production URL or
/// credential fallback in the release adapter.
pub(crate) fn local(repo: &Path, bare: &Path) -> Remote {
    let slug = Slug::parse("fixture/app").unwrap();
    let client = GithubBuilder::new(Router::empty())
        .git_remote_for_tests("fixture/app", bare)
        .build();
    Remote::new(client, slug, repo).unwrap()
}

fn repo(root: &Path, name: &str, bare: bool) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir(&path).unwrap();
    let args = if bare {
        vec!["init", "--bare", "-q"]
    } else {
        vec!["init", "-q", "-b", "main"]
    };
    git(&path, &args);
    path
}

fn commit(repo: &Path, text: &str) -> String {
    std::fs::write(repo.join("line.toml"), text).unwrap();
    git(repo, &["add", "line.toml"]);
    git(repo, &["commit", "-q", "-m", "fixture"]);
    git(repo, &["rev-parse", "HEAD"])
}

/// The name and peeled commit alone cannot bind an annotated historical tag:
/// rewriting only its message must invalidate qualify's saved admission.
#[test]
fn rewritten_historical_tag_refuses_saved_commit_gates() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    commit(&source, "release source\n");
    git(&source, &["tag", "-a", "v1", "-m", "original"]);
    git(
        &source,
        &["push", "-q", bare.to_str().unwrap(), "main", "--tags"],
    );
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let remote = local(&source, &bare);
    let runner = GitCli::new(&source);
    let record = |tags| crate::gates::CommitGateRecord::new("source", "1.0.0", "seal".into(), tags);
    record(remote.tag_fingerprint().unwrap())
        .save(&runner)
        .unwrap();
    record(remote.tag_fingerprint().unwrap())
        .adopt(&runner)
        .unwrap();
    git(
        &source,
        &["tag", "-a", "replacement", "-m", "rewritten message"],
    );
    let changed = git(&source, &["rev-parse", "replacement"]);
    // Import the replacement body into the bare fixture before asking ls-remote
    // to peel it. Its final commit is identical to v1's original final commit.
    git(
        &source,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            "refs/tags/replacement",
        ],
    );
    // Mutate only the owned bare fixture; no force push or real remote.
    git(&bare, &["update-ref", "refs/tags/v1", &changed]);
    git(&bare, &["update-ref", "-d", "refs/tags/replacement"]);
    assert!(
        record(remote.tag_fingerprint().unwrap())
            .adopt(&runner)
            .is_err()
    );
}

fn assert_new_native_children_are_progress_bound(remote: &Remote, first: usize) {
    let children = remote.client.fx().children_run();
    let children = &children[first..];
    assert!(!children.is_empty(), "the native operation ran no child");
    assert!(
        children
            .iter()
            .any(|child| child.args.iter().any(|arg| arg == "push")),
        "the native operation never reached its push: {children:?}"
    );
    for child in children {
        let progress = child
            .progress
            .as_ref()
            .expect("progress-bound native child");
        assert_eq!(progress.window, STALL_WINDOW);
        assert!(
            progress.waiting_on.starts_with("git "),
            "{}",
            progress.waiting_on
        );
    }
}

/// The release adapter binds only its explicit agh.toml route even when its environment holds
/// both ambient token names. The complete origin refresh then uses that configured route.
/// NEGATIVE CONTROL: consulting an ambient identity instead of the configured route changes
/// `GitOut.as_`; the agh runner's executed-child control separately proves that ambient variables
/// cannot enter a routed Git process.
#[test]
fn configured_origin_refresh_ignores_dead_ambient_tokens() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let main = commit(&source, "configured remote\n");
    git(&source, &["tag", "v1"]);
    git(
        &source,
        &["push", "-q", bare.to_str().unwrap(), "main", "--tags"],
    );

    let target = repo(root.path(), "target", false);
    commit(&target, "local checkout\n");
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );

    let state = root.path().join("agh-state");
    aterm_uds::private_file::create_private_dir(&state).unwrap();
    let dirs = agh::config::Dirs::under(&state);
    aterm_uds::private_file::create_private_dir(&dirs.home).unwrap();
    std::fs::create_dir_all(dirs.config.parent().unwrap()).unwrap();
    aterm_uds::private_file::create_private_dir(&dirs.config).unwrap();
    aterm_uds::private_file::write_private(
        &dirs.home,
        "reader.token",
        agh_fake::token("configured-reader").as_bytes(),
    )
    .unwrap();
    let config = r#"
        [profiles.reader]
        token_file = "~/reader.token"
        tier = "dev"
        login = "fixture-reader"
        [[owners]]
        name = "fixture"
        tier = "dev"
        read = ["reader"]
        write = "reader"
        [default]
        read = ["reader"]
        "#;
    aterm_uds::private_file::write_private(
        &dirs.config,
        dirs.config_file().file_name().unwrap().to_str().unwrap(),
        config.as_bytes(),
    )
    .unwrap();

    let dead = agh_fake::token("configured-dead-ambient");
    let env = agh::identity::MapEnv::new()
        .with("PATH", &std::env::var("PATH").unwrap())
        .with("GH_TOKEN", &dead)
        .with("GITHUB_TOKEN", &dead);
    let helper = agh::git::HelperCommand {
        exe: "/usr/bin/false".into(),
        args: Vec::new(),
    };
    let base = GithubBuilder::new(Router::empty())
        .env(Arc::new(env))
        .fx(Arc::new(agh::Fx::live()))
        .journal(dirs.clone())
        .git_helper_for_tests(helper)
        .git_remote_for_tests("fixture/app", &bare)
        .build();
    let client = bind_configured(&dirs, &base).unwrap();
    let slug = Slug::parse("fixture/app").unwrap();
    let routed = client
        .run_git(&GitRun::ls_remote(&slug, Some("refs/heads/main")))
        .unwrap();
    assert_eq!(routed.as_.as_deref(), Some("reader"));

    let remote = Remote::new(client, slug, &target).unwrap();
    let fetched = remote.fetch_origin().unwrap();
    assert_eq!(fetched.main, main);
    assert_eq!(git(&target, &["rev-parse", ORIGIN_MAIN]), main);
}

#[test]
fn native_origin_refresh_records_only_main_and_preserves_annotated_tag_peels() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let old = commit(&source, "old remote\n");
    git(&source, &["tag", "v1"]);
    git(
        &source,
        &["push", "-q", bare.to_str().unwrap(), "main", "--tags"],
    );

    let target = repo(root.path(), "target", false);
    let local_head = commit(&target, "local checkout\n");
    git(&target, &["tag", "local-only"]);
    git(
        &target,
        &["update-ref", "refs/remotes/other/main", &local_head],
    );
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let gitdir = target.join(".git");
    std::fs::write(gitdir.join("FETCH_HEAD"), "do not replace this\n").unwrap();
    let files = ["HEAD", "index", "config", "FETCH_HEAD"];
    let before = files.map(|name| std::fs::read(gitdir.join(name)).unwrap());
    let remote = local(&target, &bare);
    let first = remote.fetch_origin().unwrap();
    assert_eq!(first.main, old);
    assert_eq!(git(&target, &["rev-parse", ORIGIN_MAIN]), old);
    assert_eq!(
        parse_tags(&first.tags).unwrap().get("refs/tags/v1"),
        Some(&old)
    );

    let new = commit(&source, "new remote\n");
    git(&source, &["tag", "-a", "v2", "-m", "annotated"]);
    let annotated = git(&source, &["rev-parse", "refs/tags/v2"]);
    assert_ne!(annotated, new);
    git(&source, &["update-ref", "refs/tags/v2-alias", &annotated]);
    git(&source, &["tag", "v1-lookalike"]);
    git(
        &source,
        &["push", "-q", bare.to_str().unwrap(), "main", "--tags"],
    );
    let advertised = git(&bare, &["show-ref"]);
    let fetched = remote.fetch_origin().unwrap();
    assert_eq!(fetched.main, new);
    let tags = parse_tags(&fetched.tags).unwrap();
    assert_eq!(tags.get("refs/tags/v2"), Some(&annotated));
    assert_eq!(tags.get("refs/tags/v2^{}"), Some(&new));
    assert_eq!(tags.get("refs/tags/v2-alias"), Some(&annotated));
    assert_eq!(tags.get("refs/tags/v2-alias^{}"), Some(&new));
    assert_eq!(remote.tag("v1").unwrap(), Some(old));
    assert_eq!(remote.tag("v2").unwrap(), Some(new.clone()));
    assert_eq!(remote.tag("v2-alias").unwrap(), Some(new.clone()));
    assert_eq!(remote.tag("absent").unwrap(), None);
    for invalid in ["v1*", "../main", "v2^{}"] {
        assert!(remote.tag(invalid).is_err());
    }
    assert_eq!(git(&target, &["rev-parse", ORIGIN_MAIN]), new);
    assert_eq!(git(&target, &["rev-parse", "HEAD"]), local_head);
    assert_eq!(
        git(&target, &["rev-parse", "refs/remotes/other/main"]),
        local_head
    );
    assert_eq!(git(&target, &["tag", "--list"]), "local-only");
    let refs_before = git(&target, &["for-each-ref"]);
    let copy = root.path().join("owned-engine");
    let (copied, captured) = remote.copy_main(&copy).unwrap();
    assert_eq!(captured, new);
    assert_eq!(git(&copy, &["rev-parse", "HEAD"]), captured);
    assert_eq!(
        git(&copy, &["rev-parse", "HEAD^{tree}"]),
        git(&bare, &["rev-parse", "main^{tree}"])
    );
    assert_eq!(
        std::fs::read(copy.join("line.toml")).unwrap(),
        b"new remote\n"
    );
    assert_eq!(copied.repository(), remote.repository());
    assert!(
        remote.copy_main(&copy).is_err(),
        "an existing destination is never reused"
    );
    assert!(remote.copy_main(&target.join("inside")).is_err());
    assert!(!target.join("inside").exists());
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&copy, &alias).unwrap();
    assert!(
        remote.copy_main(&alias).is_err(),
        "a destination symlink is not owned output"
    );
    assert_eq!(git(&target, &["for-each-ref"]), refs_before);
    assert_eq!(
        before,
        files.map(|name| std::fs::read(gitdir.join(name)).unwrap())
    );
    assert_eq!(git(&bare, &["show-ref"]), advertised);
}

#[test]
fn typed_origin_keeps_atomic_updates_expected_deletes_and_immutable_tag_reads() {
    use crate::ledger::Origin;
    use agh::github::refs::{AtomicRefBatch, RefUpdate};

    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let old = commit(&source, "remote main\n");
    git(&source, &["tag", "-a", "fence", "-m", "owned fence"]);
    let token = git(&source, &["rev-parse", "refs/tags/fence"]);
    git(
        &source,
        &["push", "-q", bare.to_str().unwrap(), "main", "--tags"],
    );
    let new = commit(&source, "local next\n");
    git(
        &source,
        &["tag", "-a", "substitute", "-m", "replacement only"],
    );
    let substitute = git(&source, &["rev-parse", "refs/tags/substitute"]);
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let remote = claim_fixture(&source, &bare);
    let origin: &dyn Origin = &remote;
    assert_eq!(origin.peel(&old).unwrap(), None);
    assert_eq!(origin.peel(&token).unwrap(), Some(old.clone()));
    let tree = git(&source, &["rev-parse", &format!("{old}^{{tree}}")]);
    git(
        &source,
        &["tag", "-a", "tree-token", "-m", "not a claim", &tree],
    );
    let tree_token = git(&source, &["rev-parse", "refs/tags/tree-token"]);
    assert!(
        origin.peel(&tree_token).is_err(),
        "a tag of a tree is not a claim"
    );
    assert_eq!(origin.get("refs/tags/fence").unwrap(), Some(token.clone()));
    origin.import(&token).unwrap();
    git(&source, &["replace", &token, &substitute]);
    assert_eq!(origin.peel(&token).unwrap(), Some(old.clone()));
    let body = String::from_utf8(origin.tag_object(&token).unwrap()).unwrap();
    assert!(body.contains("owned fence"));
    assert!(!body.contains("replacement only"));
    assert!(origin.tag_object(&new).is_err());
    assert!(origin.peel("HEAD").is_err());
    git(&source, &["replace", "-d", &token]);
    assert_eq!(origin.refresh_main().unwrap(), old);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), new);

    let owner = "refs/heads/control-owner";
    let fence = "refs/heads/control-fence";
    let batch = |before_owner: &str, after_owner: &str, before_fence: &str, after_fence: &str| {
        AtomicRefBatch::new(vec![
            RefUpdate {
                reference: owner.into(),
                before: before_owner.into(),
                after: after_owner.into(),
            },
            RefUpdate {
                reference: fence.into(),
                before: before_fence.into(),
                after: after_fence.into(),
            },
        ])
        .unwrap()
    };
    assert_eq!(
        remote
            .write_batch(batch(NULL_OID, &old, NULL_OID, &new))
            .unwrap(),
        RefWrite::Written
    );
    assert_eq!(origin.get(owner).unwrap(), Some(old.clone()));
    assert_eq!(origin.get(fence).unwrap(), Some(new.clone()));
    // One matching member cannot publish when its sibling's expected OID is wrong.
    assert_eq!(
        remote.write_batch(batch(&old, &new, &old, &new)).unwrap(),
        RefWrite::Conflict
    );
    assert_eq!(origin.get(owner).unwrap(), Some(old.clone()));
    assert_eq!(origin.get(fence).unwrap(), Some(new.clone()));
    assert_eq!(origin.delete(owner, &new).unwrap(), RefWrite::Conflict);
    assert_eq!(origin.get(owner).unwrap(), Some(old.clone()));
    assert_eq!(origin.delete(owner, &old).unwrap(), RefWrite::Written);
    assert_eq!(origin.get(owner).unwrap(), None);
    assert_eq!(origin.write(owner, None, &old).unwrap(), RefWrite::Written);
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/foreign/app.git",
        ],
    );
    assert!(origin.delete(fence, &new).is_err());
    assert_eq!(git(&bare, &["rev-parse", owner]), old);
    assert_eq!(git(&bare, &["rev-parse", fence]), new);
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    std::fs::rename(&bare, root.path().join("offline.git")).unwrap();
    assert!(
        origin.get(fence).is_err(),
        "cached objects cannot establish a current ref"
    );
    origin.import(&token).unwrap();
    assert!(origin.import(&"f".repeat(40)).is_err());

    // Borrowing a complete immutable graph needs no remote transfer. A lone
    // tag object still cannot establish its missing commit/tree graph.
    let incomplete = repo(root.path(), "root-only", false);
    git(
        &incomplete,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let object = PathBuf::from(".git/objects")
        .join(&token[..2])
        .join(&token[2..]);
    std::fs::create_dir_all(incomplete.join(&object).parent().unwrap()).unwrap();
    std::fs::copy(source.join(&object), incomplete.join(&object)).unwrap();
    assert_eq!(git(&incomplete, &["cat-file", "-t", &token]), "tag");
    let incomplete_remote = claim_fixture(&incomplete, &bare);
    assert!(
        incomplete_remote.fetch_exact(&token).is_err(),
        "a root object without its graph cannot be imported offline"
    );
    assert_eq!(origin.peel(&token).unwrap(), Some(old));
}

#[test]
fn a_persisted_unknown_native_intent_blocks_the_adapter_after_restart() {
    use agh::journal::{IntentDraft, IntentStore, Settled};
    use agh::policy::{RouteRequest, RouteTarget};
    use agh::write::WriteOp;

    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let old = commit(&source, "remote main\n");
    git(&source, &["push", "-q", bare.to_str().unwrap(), "main"]);
    let new = commit(&source, "next main\n");
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let remote = claim_fixture(&source, &bare);
    let request = RouteRequest::write(RouteTarget::Repo(remote.slug.clone()));
    let permit = remote.client.permit(&request).unwrap();
    let journal = remote.client.journal(&remote.slug).unwrap();
    let bytes = b"earlier native atomic push lost its answer";
    let digest = agh::model::Sha256Digest::of(bytes).to_string();
    let draft = IntentDraft::new(
        WriteOp::RefUpdateAtomic,
        &remote.slug,
        &permit,
        &digest,
        "atomic refs",
    )
    .with_request(format!("{digest} {}", bytes.len()))
    .with_body_digest(Some(digest));
    let id = journal.begin(&draft).unwrap();
    journal
        .settle(
            &id,
            &Settled::Unknown {
                why: "fixture lost answer".into(),
            },
        )
        .unwrap();
    drop(journal);
    drop(remote);

    // Rebuild the actual native adapter over the same persisted journal. A raw
    // run_git push would ignore the pending intent and move this real bare ref.
    let restarted = claim_fixture(&source, &bare);
    let error = restarted
        .write_ref("refs/heads/main", Some(&old), &new)
        .unwrap_err();
    assert!(error.contains(id.as_str()), "{error}");
    assert_eq!(git(&bare, &["rev-parse", "refs/heads/main"]), old);
    let reopened = restarted.client.journal(&restarted.slug).unwrap();
    assert!(
        reopened
            .open()
            .unwrap()
            .iter()
            .any(|intent| intent.id == id)
    );
    let control = crate::ledger::ControlRefTransaction::new(
        &old,
        vec![crate::ledger::ControlRefChange {
            reference: agh::git::RELEASE_LEASE_REF.into(),
            before: None,
            after: Some(old.clone()),
        }],
    )
    .unwrap();
    let error = crate::ledger::Origin::control_transaction(&restarted, control).unwrap_err();
    assert!(error.contains(id.as_str()), "{error}");
    assert!(
        git(&bare, &["show-ref"])
            .lines()
            .all(|row| !row.ends_with(agh::git::RELEASE_LEASE_REF))
    );
}

#[test]
fn canonical_control_wire_rotates_owned_tokens_and_keeps_unlock_atomic() {
    use crate::ledger::{ControlRefChange, ControlRefTransaction, Origin};
    use agh::git::{PUBLISHER_FENCE_REF as FENCE, RELEASE_LEASE_REF as LEASE};
    use agh::github::refs::{AtomicRefBatch, RefUpdate};

    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let owner = commit(&source, "claim owner\n");
    git(&source, &["push", "-q", bare.to_str().unwrap(), "main"]);
    git(
        &source,
        &["tag", "-a", "first", "-m", "first token", &owner],
    );
    git(
        &source,
        &["tag", "-a", "second", "-m", "second token", &owner],
    );
    let first = git(&source, &["rev-parse", "refs/tags/first"]);
    let second = git(&source, &["rev-parse", "refs/tags/second"]);
    let foreign = commit(&source, "another claim\n");
    git(
        &source,
        &["tag", "-a", "foreign", "-m", "foreign token", &foreign],
    );
    let foreign_token = git(&source, &["rev-parse", "refs/tags/foreign"]);
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let remote = claim_fixture(&source, &bare);
    let origin: &dyn Origin = &remote;
    let row = |reference: &str, before: Option<&str>, after: Option<&str>| ControlRefChange {
        reference: reference.into(),
        before: before.map(str::to_owned),
        after: after.map(str::to_owned),
    };
    let transaction = |changes| ControlRefTransaction::new(&owner, changes).unwrap();
    let create = transaction(vec![row(LEASE, None, Some(&owner))]);
    let mut unjournaled = GitRun::check_local(&remote.slug, &source);
    unjournaled.op = agh::git::GitOp::PushControl {
        dir: source.clone(),
        transaction: create.clone(),
        dry_run: false,
    };
    assert!(
        remote
            .client
            .run_git(&unjournaled)
            .unwrap_err()
            .why
            .contains("journaled")
    );
    assert_eq!(origin.get(LEASE).unwrap(), None);
    let first_child = remote.client.fx().children_run().len();
    assert_eq!(
        origin.control_transaction(create).unwrap(),
        RefWrite::Written
    );
    assert_new_native_children_are_progress_bound(&remote, first_child);

    let ordinary_ref = "refs/heads/progress-bound";
    let ordinary = AtomicRefBatch::new(vec![RefUpdate {
        reference: ordinary_ref.into(),
        before: NULL_OID.into(),
        after: owner.clone(),
    }])
    .unwrap();
    let first_child = remote.client.fx().children_run().len();
    assert_eq!(remote.write_batch(ordinary).unwrap(), RefWrite::Written);
    assert_new_native_children_are_progress_bound(&remote, first_child);
    let first_child = remote.client.fx().children_run().len();
    assert_eq!(
        remote.delete_ref(ordinary_ref, &owner).unwrap(),
        RefWrite::Written
    );
    assert_new_native_children_are_progress_bound(&remote, first_child);

    assert_eq!(
        origin
            .control_transaction(transaction(vec![row(FENCE, None, Some(&first))]))
            .unwrap(),
        RefWrite::Written
    );

    let ordinary = AtomicRefBatch::new(vec![RefUpdate {
        reference: FENCE.into(),
        before: first.clone(),
        after: second.clone(),
    }])
    .unwrap();
    assert!(
        remote.write_batch(ordinary).is_err(),
        "ordinary tags must remain immutable"
    );
    assert_eq!(origin.get(FENCE).unwrap(), Some(first.clone()));
    assert!(
        origin
            .control_transaction(transaction(vec![row(
                FENCE,
                Some(&first),
                Some(&foreign_token)
            )]))
            .is_err()
    );
    assert_eq!(origin.get(FENCE).unwrap(), Some(first.clone()));
    assert_eq!(
        origin
            .control_transaction(transaction(vec![row(FENCE, Some(&first), Some(&second))]))
            .unwrap(),
        RefWrite::Written
    );

    // A wrong fence lease cannot partially remove the otherwise matching owner.
    assert_eq!(
        origin
            .control_transaction(transaction(vec![
                row(LEASE, Some(&owner), None),
                row(FENCE, Some(&first), None)
            ]))
            .unwrap(),
        RefWrite::Conflict
    );
    assert_eq!(origin.get(LEASE).unwrap(), Some(owner.clone()));
    assert_eq!(origin.get(FENCE).unwrap(), Some(second.clone()));
    assert_eq!(
        origin
            .control_transaction(transaction(vec![
                row(LEASE, Some(&owner), None),
                row(FENCE, Some(&second), None)
            ]))
            .unwrap(),
        RefWrite::Written
    );
    assert_eq!(origin.get(LEASE).unwrap(), None);
    assert_eq!(origin.get(FENCE).unwrap(), None);

    assert_eq!(
        origin
            .control_transaction(transaction(vec![row(LEASE, None, Some(&owner))]))
            .unwrap(),
        RefWrite::Written
    );
    assert_eq!(
        origin
            .control_transaction(transaction(vec![row(FENCE, None, Some(&first))]))
            .unwrap(),
        RefWrite::Written
    );
    // An external lease loss must not prevent exact cleanup of this old token.
    git(&bare, &["update-ref", "-d", LEASE, &owner]);
    assert_eq!(
        origin
            .control_transaction(transaction(vec![row(FENCE, Some(&first), None)]))
            .unwrap(),
        RefWrite::Written
    );
    assert_eq!(origin.get(FENCE).unwrap(), None);
    assert_eq!(origin.get(LEASE).unwrap(), None);
}

#[test]
fn a_complete_tag_catalog_needs_no_main_and_tag_commits_refuse_noncommits() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let commit = commit(&source, "tagged release\n");
    git(&source, &["tag", "-a", "v1", "-m", "release", &commit]);
    let token = git(&source, &["rev-parse", "refs/tags/v1"]);
    git(&source, &["update-ref", "refs/tags/alias", &token]);
    let tree = git(&source, &["rev-parse", &format!("{commit}^{{tree}}")]);
    git(
        &source,
        &[
            "tag",
            "-a",
            "tree-only",
            "-m",
            "not a release commit",
            &tree,
        ],
    );
    git(&source, &["push", "-q", bare.to_str().unwrap(), "--tags"]);

    let target = repo(root.path(), "target", false);
    let head = self::commit(&target, "owned checkout\n");
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let gitdir = target.join(".git");
    std::fs::write(gitdir.join("FETCH_HEAD"), "preserve this observation\n").unwrap();
    let names = ["HEAD", "index", "config", "FETCH_HEAD"];
    let before = names.map(|name| std::fs::read(gitdir.join(name)).unwrap());
    let remote = local(&target, &bare);
    assert_eq!(remote.get("refs/heads/main").unwrap(), None);
    let catalog = parse_tags(&remote.tag_catalog().unwrap()).unwrap();
    assert_eq!(catalog.get("refs/tags/v1"), Some(&token));
    assert_eq!(catalog.get("refs/tags/v1^{}"), Some(&commit));
    assert_eq!(catalog.get("refs/tags/alias^{}"), Some(&commit));
    assert_eq!(remote.tag_commit("v1").unwrap(), Some(commit));
    assert_eq!(remote.tag_commit("absent").unwrap(), None);
    assert!(remote.tag_commit("tree-only").is_err());
    assert_eq!(git(&target, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        before,
        names.map(|name| std::fs::read(gitdir.join(name)).unwrap())
    );
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/foreign/app.git",
        ],
    );
    assert!(
        remote.tag_catalog().is_err(),
        "a captured source cannot silently retarget"
    );
}

#[test]
fn a_baseline_imports_only_the_first_advertised_preference_and_proves_its_commit() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    let first = commit(&source, "first release\n");
    git(&source, &["tag", "-a", "v1", "-m", "release", &first]);
    let later = commit(&source, "unselected release\n");
    git(&source, &["tag", "v2", &later]);
    let tree = git(&source, &["rev-parse", &format!("{later}^{{tree}}")]);
    git(&source, &["tag", "tree-only", &tree]);
    git(&source, &["push", "-q", bare.to_str().unwrap(), "--tags"]);
    let target = repo(root.path(), "target", false);
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let remote = local(&target, &bare);
    let preferences = |names: &[&str]| {
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        remote
            .first_tag_commit(&preferences(&["absent", "v1", "v2"]))
            .unwrap(),
        Some(("v1".into(), first.clone()))
    );
    // NEGATIVE CONTROL: importing the whole catalog also transfers this object.
    let mut absent = object_git(&target).unwrap();
    absent.args(["cat-file", "-e", &later]);
    assert!(
        !absent.status().unwrap().success(),
        "an unselected tag was imported"
    );
    assert_eq!(
        remote
            .first_tag_commit(&preferences(&["v2", "v1"]))
            .unwrap(),
        Some(("v2".into(), later))
    );
    assert!(
        remote
            .first_tag_commit(&preferences(&["tree-only", "v1"]))
            .is_err()
    );
    assert_eq!(
        remote.first_tag_commit(&preferences(&["absent"])).unwrap(),
        None
    );
    std::fs::rename(&bare, root.path().join("offline.git")).unwrap();
    assert!(
        remote.first_tag_commit(&preferences(&["v1"])).is_err(),
        "an imported object is no current tag observation"
    );
}

#[test]
fn child_branch_and_remote_head_import_exact_commits_without_fetch_head() {
    let root = aterm_tempfile::tempdir().unwrap();
    let parent = repo(root.path(), "parent", false);
    commit(&parent, "parent\n");
    let parent_bare = repo(root.path(), "parent.git", true);
    git(
        &parent,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    std::fs::create_dir(parent.join("vendor")).unwrap();
    let child = repo(&parent.join("vendor"), "astream", false);
    let old = commit(&child, "old child\n");
    let tip = commit(&child, "current child\n");
    let bare = repo(root.path(), "child.git", true);
    git(&child, &["push", "-q", bare.to_str().unwrap(), "main"]);
    git(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&child, &["checkout", "-q", "--detach", &old]);
    git(
        &child,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let gitdir = child.join(".git");
    std::fs::write(gitdir.join("FETCH_HEAD"), "keep the child observation\n").unwrap();
    let names = ["HEAD", "index", "config", "FETCH_HEAD"];
    let before = names.map(|name| std::fs::read(gitdir.join(name)).unwrap());
    let remote = local(&parent, &parent_bare);
    let path = Path::new("vendor/astream");
    for reference in ["refs/heads/main", "HEAD"] {
        let observed = remote
            .submodule_tip_with(path, reference, |repo| Ok(local(repo, &bare)))
            .unwrap();
        assert_eq!(
            observed, tip,
            "the remote tip is distinct from the detached local HEAD"
        );
    }
    assert_eq!(
        before,
        names.map(|name| std::fs::read(gitdir.join(name)).unwrap())
    );
    assert_eq!(git(&child, &["rev-parse", "HEAD"]), old);
    for path in ["", "..", "../child", "/outside", "vendor/../astream"] {
        assert!(
            remote
                .submodule_tip_with(Path::new(path), "HEAD", |_| panic!(
                    "invalid path must not open a child"
                ))
                .is_err()
        );
    }
    for reference in ["main", "refs/tags/v1", "HEAD^{}", "refs/heads/a*"] {
        assert!(
            remote
                .submodule_tip_with(path, reference, |_| panic!(
                    "invalid ref must not open a child"
                ))
                .is_err()
        );
    }
    assert!(
        remote
            .submodule_tip_with(path, "refs/heads/absent", |repo| Ok(local(repo, &bare)))
            .is_err()
    );
    assert!(
        remote
            .submodule_tip_with(path, "HEAD", |repo| {
                let admitted = local(repo, &bare);
                git(
                    repo,
                    &[
                        "config",
                        "remote.origin.url",
                        "https://github.com/foreign/app.git",
                    ],
                );
                Ok(admitted)
            })
            .is_err()
    );
    git(
        &child,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    assert!(
        remote
            .submodule_tip_with(path, "HEAD", |repo| {
                let admitted = local(repo, &bare);
                git(
                    &parent,
                    &[
                        "config",
                        "remote.origin.url",
                        "https://github.com/foreign/app.git",
                    ],
                );
                Ok(admitted)
            })
            .is_err(),
        "the parent authority is rechecked after the child import"
    );
    git(
        &parent,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.path(), parent.join("vendor/escape")).unwrap();
        assert!(
            remote
                .submodule_tip_with(Path::new("vendor/escape"), "HEAD", |_| panic!(
                    "escaped path must not open a child"
                ))
                .is_err()
        );
    }
}

#[test]
fn head_answers_require_exact_complete_single_raw_oid_rows() {
    let oid = "1".repeat(40);
    assert_eq!(parse_head(&format!("{oid}\tHEAD\n")).unwrap(), oid);
    for text in [
        String::new(),
        format!("{}\tHEAD\n", "0".repeat(40)),
        format!("{oid}\trefs/heads/main\n"),
        "not-an-oid\tHEAD\n".into(),
        format!("{oid}\tHEAD\n{oid}\tHEAD\n"),
        format!("{oid}\tHEAD\r\n"),
        format!("{oid}\tHEAD\0\n"),
        format!("{}\tHEAD\n", "1".repeat(COMPLETE_LIMIT)),
    ] {
        assert!(
            parse_head(&text).is_err(),
            "malformed HEAD answer was accepted"
        );
    }
}

#[test]
fn refresh_refuses_missing_main_symbolic_tracking_and_concurrent_ref_changes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let target = repo(root.path(), "target", false);
    let head = commit(&target, "owned checkout\n");
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let remote = local(&target, &bare);
    let before = git(&target, &["show-ref"]);
    assert!(
        remote
            .fetch_origin()
            .err()
            .unwrap()
            .contains("no main branch")
    );
    assert_eq!(git(&target, &["show-ref"]), before);

    let source = repo(root.path(), "source", false);
    let old = commit(&source, "old\n");
    let new = commit(&source, "new\n");
    git(&source, &["push", "-q", bare.to_str().unwrap(), "main"]);
    remote.fetch_exact(&old).unwrap();
    remote.fetch_exact(&new).unwrap();
    remote.record_main(None, &old).unwrap();
    let expected = remote.tracking_main().unwrap();
    git(&target, &["update-ref", ORIGIN_MAIN, &head]);
    assert!(
        remote
            .record_main(expected.as_deref(), &new)
            .unwrap_err()
            .contains("changed")
    );
    assert_eq!(git(&target, &["rev-parse", ORIGIN_MAIN]), head);
    assert!(remote.record_main(None, &new).is_err());

    git(&target, &["update-ref", ORIGIN_MAIN, &old]);
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/foreign.git",
        ],
    );
    assert!(remote.record_main(Some(&old), &new).is_err());
    assert_eq!(git(&target, &["rev-parse", ORIGIN_MAIN]), old);
    assert_eq!(git(&target, &["rev-parse", "HEAD"]), head);
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    git(&target, &["symbolic-ref", ORIGIN_MAIN, "refs/heads/main"]);
    assert!(remote.fetch_origin().err().unwrap().contains("symbolic"));
    assert_eq!(
        git(&target, &["symbolic-ref", ORIGIN_MAIN]),
        "refs/heads/main"
    );
    assert_eq!(git(&target, &["rev-parse", "HEAD"]), head);
    assert!(remote.record_main(Some(&head), &new).is_err());
    assert_eq!(git(&target, &["rev-parse", "HEAD"]), head);
}

#[test]
fn malformed_partial_or_foreign_tag_ads_cannot_establish_absence_or_echo_payloads() {
    let oid = "1".repeat(40);
    for text in [
        "x".repeat(COMPLETE_LIMIT),
        format!("{oid}\trefs/heads/private-payload\n"),
        format!("{oid}\trefs/tags/v1\n{oid}\trefs/tags/v1\n"),
        format!("{oid}\trefs/tags/v1^{{}}\n"),
        format!("{oid}\trefs/tags/v1^{{}}^{{}}\n"),
        format!("{oid} refs/tags/v1\n"),
        format!("{}\trefs/tags/v1\n", "0".repeat(39)),
        format!("{oid}\trefs/tags/\n"),
    ] {
        let error = parse_tags(&text).unwrap_err();
        assert!(!error.contains("private-payload"));
    }
    assert!(parse_tags("").unwrap().is_empty());
}

#[test]
fn helper_forwarding_accepts_only_the_actual_credential_protocol() {
    let words = |args: &[&str]| {
        args.iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
    };
    for operation in ["get", "store", "erase"] {
        assert_eq!(
            helper_arguments(&words(&["agh", "auth", "git-credential", operation])).unwrap(),
            Some(words(&["auth", "git-credential", operation]))
        );
    }
    assert_eq!(
        helper_arguments(&words(&["release", "status", "--fleet"])).unwrap(),
        None
    );
    for args in [
        vec!["agh"],
        vec!["agh", "api", "user"],
        vec!["agh", "auth", "git-credential", "get", "extra"],
        vec!["agh", "auth", "git-credential", "unknown"],
    ] {
        assert!(helper_arguments(&words(&args)).is_err());
    }
}

#[test]
fn ambient_git_state_cannot_rebind_the_explicit_source() {
    const CASE: &str = "ATERM_RELEASE_ORIGIN_REDIRECT";
    const SOURCE: &str = "ATERM_RELEASE_ORIGIN_SOURCE";
    const NAME: &str = "github_remote::tests::ambient_git_state_cannot_rebind_the_explicit_source";
    if let Some(case) = std::env::var_os(CASE) {
        let source = PathBuf::from(std::env::var_os(SOURCE).unwrap());
        let ordinary = GitCli::new(&source)
            .git(&["remote", "get-url", "origin"])
            .unwrap();
        assert!(ordinary.success());
        let redirected = if case == "dir" {
            "https://example.invalid/fixture/redirect.git"
        } else {
            "https://github.com/fixture/redirect.git"
        };
        assert_eq!(
            ordinary.stdout_utf8().trim(),
            redirected,
            "negative control must activate the inherited Git redirect"
        );
        assert_eq!(origin_slug(&source).unwrap().key(), "fixture/actual");
        println!("entered origin redirect: {}", case.to_str().unwrap());
        return;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let global = root.path().join("global.gitconfig");
    std::fs::write(
        &global,
        "[url \"https://example.invalid/\"]\n    insteadOf = https://github.com/\n",
    )
    .unwrap();
    let source = repo(root.path(), "source", false);
    let other = repo(root.path(), "other", false);
    git(
        &source,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/actual.git",
        ],
    );
    git(
        &other,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/redirect.git",
        ],
    );
    for case in ["dir", "config"] {
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args(["--exact", NAME, "--test-threads=1", "--nocapture"])
            .env(CASE, case)
            .env(SOURCE, &source);
        for (key, _) in std::env::vars_os() {
            if key.as_encoded_bytes().starts_with(b"GIT_") {
                child.env_remove(key);
            }
        }
        // Own the global rewrite too: a host's HTTPS-to-SSH rule must not decide
        // this negative control. The command-config case's longer prefix wins.
        child
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &global)
            .env(crate::ledger::testing::CHOSEN_GIT_ENV, "1");
        if case == "dir" {
            child.env("GIT_DIR", other.join(".git"));
        } else {
            // Origin URLs are multivalued; a later URL does not replace the
            // first. Activate the config redirect through Git's actual rewrite.
            child
                .env("GIT_CONFIG_COUNT", "1")
                .env(
                    "GIT_CONFIG_KEY_0",
                    "url.https://github.com/fixture/redirect.git.insteadOf",
                )
                .env(
                    "GIT_CONFIG_VALUE_0",
                    "https://github.com/fixture/actual.git",
                );
        }
        let out = aterm_uds::deadline::output_within(&mut child, Duration::from_secs(60)).unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(
            out.status.success()
                && stdout
                    .lines()
                    .any(|line| line.ends_with(&format!("entered origin redirect: {case}"))),
            "owned exact test must actually enter and pass: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn native_reads_preserve_exact_raw_objects_and_all_source_state() {
    let root = aterm_tempfile::tempdir().unwrap();
    let bare = repo(root.path(), "remote.git", true);
    let source = repo(root.path(), "source", false);
    std::fs::write(source.join("line.toml"), "raw fleet state\n").unwrap();
    git(&source, &["add", "line.toml"]);
    git(&source, &["commit", "-q", "-m", "exact state"]);
    let oid = git(&source, &["rev-parse", "HEAD"]);
    let reference = "refs/aterm-release/line/v0.158.0";
    git(
        &source,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            &format!("{oid}:{reference}"),
        ],
    );
    let target = repo(root.path(), "target", false);
    git(
        &target,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    let gitdir = target.join(".git");
    std::fs::write(gitdir.join("FETCH_HEAD"), "do not replace this\n").unwrap();
    let before =
        ["config", "HEAD", "FETCH_HEAD"].map(|name| std::fs::read(gitdir.join(name)).unwrap());
    let advertised = git(&bare, &["show-ref"]);
    let remote = local(&target, &bare);
    assert_eq!(
        remote.list("refs/aterm-release/line/").unwrap(),
        BTreeMap::from([(reference.into(), oid.clone())])
    );
    assert_eq!(remote.get(reference).unwrap(), Some(oid.clone()));
    assert_eq!(remote.get("refs/aterm-release/line/absent").unwrap(), None);
    remote.fetch_exact(&oid).unwrap();
    assert_eq!(
        git(&target, &["cat-file", "commit", &oid]),
        git(&source, &["cat-file", "commit", &oid])
    );
    assert_eq!(
        git(&target, &["show", &format!("{oid}:line.toml")]),
        "raw fleet state"
    );
    assert_eq!(
        remote.blob(&oid, Path::new("line.toml")).unwrap(),
        b"raw fleet state\n"
    );
    assert!(remote.blob(&oid, Path::new("../line.toml")).is_err());
    assert!(remote.blob(&oid, Path::new("absent")).is_err());
    assert_eq!(
        git(&target, &["for-each-ref", "--format=%(refname)"]),
        format!("refs/agh/aterm-release/object/{oid}")
    );
    assert!(!gitdir.join("index").exists());
    assert_eq!(
        before,
        ["config", "HEAD", "FETCH_HEAD"].map(|name| std::fs::read(gitdir.join(name)).unwrap())
    );
    assert_eq!(git(&bare, &["show-ref"]), advertised);
    assert!(remote.get("refs/aterm-release/line/*").is_err());
    assert!(remote.fetch_exact("HEAD").is_err());

    let original_blob = git(&target, &["rev-parse", &format!("{oid}:line.toml")]);
    let ordinary = GitCli::new(&target);
    let changed = ordinary
        .git_input(&["hash-object", "-w", "--stdin"], b"replacement state\n")
        .unwrap();
    assert!(changed.success());
    git(
        &target,
        &["replace", &original_blob, changed.stdout_utf8().trim()],
    );
    let replaced = ordinary
        .git(&["show", &format!("{oid}:line.toml")])
        .unwrap();
    assert!(replaced.success());
    assert_eq!(
        replaced.stdout, b"replacement state\n",
        "negative control must activate Git replacement"
    );
    assert_eq!(
        remote.blob(&oid, Path::new("line.toml")).unwrap(),
        b"raw fleet state\n"
    );
}

#[test]
fn rewriting_source_config_is_refused_before_any_import() {
    let root = aterm_tempfile::tempdir().unwrap();
    let target = repo(root.path(), "target", false);
    let bare = repo(root.path(), "remote.git", true);
    let client = |effects| {
        GithubBuilder::new(Router::empty())
            .fx(effects)
            .git_remote_for_tests("fixture/app", &bare)
            .build()
    };
    let effects = || Arc::new(agh::Fx::simulated(agh::Fx::live().wall_secs().unwrap()));
    let slug = Slug::parse("fixture/app").unwrap();
    drop(Remote::new(client(effects()), slug.clone(), &target).unwrap());
    git(
        &target,
        &[
            "config",
            "url.https://other.invalid/.insteadOf",
            "https://github.com/",
        ],
    );
    let config = std::fs::read(target.join(".git/config")).unwrap();
    let effects = effects();
    let error = Remote::new(client(Arc::clone(&effects)), slug, &target)
        .err()
        .unwrap();
    assert!(
        !error.contains("other.invalid"),
        "private URL leaked: {error}"
    );
    assert!(
        effects.children_run().iter().all(|child| {
            !child
                .args
                .iter()
                .any(|arg| matches!(arg.to_str(), Some("clone" | "fetch" | "ls-remote" | "push")))
        }),
        "source refusal must precede all remote Git commands"
    );
    assert_eq!(std::fs::read(target.join(".git/config")).unwrap(), config);
    assert!(git(&target, &["for-each-ref"]).is_empty());
    assert!(git(&bare, &["for-each-ref"]).is_empty());
    git(
        &target,
        &["config", "--unset", "url.https://other.invalid/.insteadOf"],
    );
    drop(local(&target, &bare));
}

#[test]
fn implicit_release_owner_has_no_grant_and_missing_identity_has_no_fallback() {
    let root = aterm_tempfile::tempdir().unwrap();
    let target = repo(root.path(), "target", false);
    let bare = repo(root.path(), "remote.git", true);
    let text = format!(
        "[profiles.writer]\ntoken_file = {:?}\nlogin = \"fixture-bot\"\ntier = \"release\"\n[[owners]]\nname = \"fixture-release\"\ntier = \"release\"\nread = [\"anonymous\"]\nwrite = \"writer\"\n",
        root.path().join("missing-token")
    );
    let parsed = agh::config::parse(&text).unwrap();
    let validated = agh::config::validate(&parsed, root.path(), &agh::config::file_id).unwrap();
    let client = GithubBuilder::new(Router::from_validated(validated)).build();
    let error = Remote::new(client, Slug::parse("fixture-release/app").unwrap(), &target)
        .err()
        .unwrap();
    assert!(error.contains("no grant"), "{error}");

    let text = text
        .replace("\"release\"", "\"dev\"")
        .replace("[\"anonymous\"]", "[\"writer\"]");
    let parsed = agh::config::parse(&text).unwrap();
    let validated = agh::config::validate(&parsed, root.path(), &agh::config::file_id).unwrap();
    let client = GithubBuilder::new(Router::from_validated(validated))
        .git_remote_for_tests("fixture-release/app", &bare)
        .build();
    let remote = Remote::new(client, Slug::parse("fixture-release/app").unwrap(), &target).unwrap();
    assert!(remote.list("refs/aterm-release/line/").is_err());
    assert!(remote.fetch_origin().is_err());
    assert!(remote.tag("v1").is_err());
    assert!(git(&target, &["for-each-ref"]).is_empty());
}

#[test]
fn incomplete_or_unrelated_advertisements_never_prove_absence() {
    let oid = "1".repeat(40);
    let prefix = "refs/aterm-release/line/";
    for text in [
        "x".repeat(COMPLETE_LIMIT),
        format!("{oid}\trefs/heads/main\n"),
        format!("{oid}\t{prefix}v1\n{oid}\t{prefix}v1\n"),
        format!("{oid} {prefix}v1\n"),
        format!("{}\t{prefix}v1\n", "0".repeat(39)),
    ] {
        assert!(parse_refs(&text, prefix).is_err());
    }
    assert!(parse_refs("", prefix).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn malformed_or_unsafe_configuration_does_not_adopt_another_identity() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let dirs = agh::config::Dirs::under(root.path());
    std::fs::create_dir_all(&dirs.config).unwrap();
    std::fs::write(dirs.config_file(), "vault = [broken\n").unwrap();
    std::fs::set_permissions(dirs.config_file(), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(configured(&dirs).is_err());
    std::fs::write(dirs.config_file(), "vault = true\n").unwrap();
    std::fs::set_permissions(dirs.config_file(), std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(configured(&dirs).is_err());
    assert!(!dirs.state.exists());
}
