// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::{GitCli, GitRunner, object_git};

fn git(repo: &Path, args: &[&str]) -> String {
    let mut cmd = object_git(repo);
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
    assert_eq!(
        before,
        files.map(|name| std::fs::read(gitdir.join(name)).unwrap())
    );
    assert_eq!(git(&bare, &["show-ref"]), advertised);
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
            .env("GIT_CONFIG_GLOBAL", &global);
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
