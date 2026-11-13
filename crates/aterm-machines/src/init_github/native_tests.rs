// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::git_credential::GITHUB;
use crate::git_credential::script;
use crate::items::defaults::tests::user;
use crate::port::{Fake, GithubResponse, Ran, Stat};

const TOKEN: &str = "fixture_development_token_only";

fn stat(directory: bool, mode: u32) -> Stat {
    Stat {
        uid: 501,
        gid: 20,
        mode,
        dir: directory,
        symlink: false,
        len: TOKEN.len() as u64,
    }
}

fn fixture() -> (Fake, NativeRepositories) {
    let (sys, item) = bare();
    sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
    script::launcher(&sys, &item.user);
    source(&sys, "/Users//alice/.secrets/gh_access_token");
    metadata(&sys, &item.user, aterm_json::json!([]));
    answer(
        &sys,
        &item.user,
        "/user",
        aterm_json::json!({"login":"alice"}),
    );
    (sys, item)
}

/// Alice's own home and nothing else: no git, no token, no GitHub answer.
fn bare() -> (Fake, NativeRepositories) {
    let mut sys = Fake::root();
    sys.euid = 501;
    let item = NativeRepositories::new(user("alice", 501), None);
    // Child publication must not invent the account's already-existing
    // ancestry and thereby change the captured source's prefix policy.
    for path in ["/", "/Users"] {
        sys.stat_of(
            path,
            Stat {
                uid: 0,
                gid: 0,
                ..stat(true, 0o755)
            },
        );
    }
    sys.stat_of(&item.user.home, stat(true, 0o700));
    (sys, item)
}

fn source(sys: &Fake, path: &str) {
    sys.file(path, TOKEN.as_bytes())
        .stat_of(path, stat(false, 0o600));
}

/// [`source`] at `user`'s synced token file, owned by `user`: a live account, in a test whose
/// git runs for real, is not the fixtures' 501.
pub(crate) fn owned_source(sys: &Fake, user: &User) {
    let path = Path::new(&user.home).join(crate::init_github::TOKEN_FILE);
    let path = path.to_str().unwrap();
    source(sys, path);
    sys.stat_of(
        path,
        Stat {
            uid: user.uid,
            gid: user.gid,
            ..stat(false, 0o600)
        },
    );
}

/// `user`'s account on `sys` listing one repository of hers, `name`, already a worktree in
/// `~`, with her synced token and aterm's route handing it back, and the git `sys` runs (the
/// distribution's on Linux): the stage init runs with no init.toml, converged.
pub(in crate::init_github) fn converged(sys: &Fake, user: &User, name: &str) {
    let git = if sys.macos() {
        crate::items::github::CLT_GIT
    } else {
        crate::items::github::SYSTEM_GIT
    };
    sys.executable(git, b"synthetic git");
    script::launcher(sys, user);
    sys.stat_of(&user.home, stat(true, 0o700));
    let home = Path::new(&user.home);
    source(
        sys,
        home.join(crate::init_github::TOKEN_FILE).to_str().unwrap(),
    );
    answer(sys, user, "/user", aterm_json::json!({"login": user.name}));
    present_repository(sys, user, name);
    script::lookups_hold(sys, user, GITHUB, &user.name, TOKEN);
    script::alike(sys, user);
}

fn present_repository(sys: &Fake, user: &User, name: &str) {
    answer(
        sys,
        user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([repository(&user.name, name, 1)]),
    );
    let path = Path::new(&user.home).join(name);
    sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
    super::repos::tests::script_existing(sys, user, &path, name);
}

#[test]
fn a_captured_local_blocker_is_not_reread_after_capture() {
    let (sys, item) = bare();
    sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let captured = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    // A later local reading could succeed. The frozen capture must still
    // report its original blocker, without reading files again.
    source(&sys, "/Users//alice/.secrets/gh_access_token");
    answer(
        &sys,
        &item.user,
        "/user",
        aterm_json::json!({"login":"alice"}),
    );
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([]),
    );
    let rows = run_captured(
        &sys,
        &ctx,
        Mode::Plan,
        RepositoryWork::Refresh,
        captured,
        Box::new(|| Ok(true)),
    )
    .unwrap();
    assert_eq!(Reading::of(stage(&rows)), Some(Reading::Waits(Wait::Token)));
    // The vault this Mac reads holds no GitHub token: a blocked operation, never a person's
    // step.
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Blocked);
    assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
}

fn answer(sys: &Fake, user: &User, endpoint: &str, body: aterm_json::Value) {
    // Default fixtures represent the same complete inventory delivered privately by the
    // vault. Explicitly scoped profiles below still exercise GitHub's transport.
    if endpoint == format!("{ACCESSIBLE}&page=1") {
        metadata(sys, user, body.clone());
    }
    sys.on_github_api(
        user,
        endpoint,
        Ok(GithubResponse {
            status: 200,
            headers: Vec::new(),
            body: aterm_json::to_vec(&body).unwrap(),
        }),
    );
}

/// `user`'s private repository inventory from the vault, listing `repositories`, owned by
/// `user`.
pub(crate) fn metadata(sys: &Fake, user: &User, repositories: aterm_json::Value) {
    let path = super::super::inventory::file(&user.home);
    let bytes = aterm_json::to_vec(&aterm_json::json!({
        "schema":2,"account":user.name,"repositories":repositories
    }))
    .unwrap();
    sys.file(path.to_str().unwrap(), &bytes).stat_of(
        path.to_str().unwrap(),
        Stat {
            uid: user.uid,
            gid: user.gid,
            len: bytes.len() as u64,
            ..stat(false, 0o600)
        },
    );
}

fn scope(sys: &Fake, user: &User) {
    let path = Path::new(&user.home).join(".config/aterm/init.toml");
    let bytes = format!("[github]\naccount='{}'\n", user.name);
    sys.file(path.to_str().unwrap(), bytes.as_bytes()).stat_of(
        path.to_str().unwrap(),
        Stat {
            len: bytes.len() as u64,
            ..stat(false, 0o600)
        },
    );
}

fn scoped_fixture() -> (Fake, NativeRepositories) {
    let (sys, item) = fixture();
    scope(&sys, &item.user);
    (sys, item)
}

pub(crate) fn repository(owner: &str, name: &str, id: u64) -> aterm_json::Value {
    aterm_json::json!({"id":id,"name":name,"owner":{"login":owner},"full_name":format!("{owner}/{name}"),"private":true,"clone_url":format!("https://github.com/{owner}/{name}.git"),"default_branch":"main"})
}

fn required_repository(sys: &Fake, who: &User, name: &str, paths: &[&str]) {
    let mut value = repository("alice", name, 1);
    value
        .as_object_mut()
        .unwrap()
        .insert("required_submodules".into(), aterm_json::json!(paths));
    metadata(sys, who, aterm_json::json!([value]));
    super::super::dependencies::tests::declarations(
        sys,
        who,
        &Path::new(&who.home).join(name),
        paths,
    );
}

/// Capability refusal is a local tool wait, and a ready managed Git resolves it without
/// changing the private inventory. Dirty dependency work remains a personal conflict.
#[test]
fn required_dependencies_wait_for_managed_git_and_preserve_dirty_work() {
    use super::super::dependencies::tests::{PIN, existing};
    use aterm_types::macos_setup::{Setting, Standing};

    let names = ["parent".to_owned()];
    let (sys, item) = listing(&names, &names);
    sys.logged_in(&item.user);
    let parent = Path::new(&item.user.home).join("parent");
    let child = parent.join("vendor/library");
    sys.stat_of(parent.join("vendor").to_str().unwrap(), stat(true, 0o700));
    sys.stat_of(child.to_str().unwrap(), stat(true, 0o700));
    sys.file(child.join(".git").to_str().unwrap(), b"inert metadata")
        .stat_of(child.join(".git").to_str().unwrap(), stat(false, 0o600));
    required_repository(&sys, &item.user, "parent", &["vendor/library"]);
    existing(&sys, &item.user, &child, "library", PIN);
    // The Command Line Tools' git is one before 2.45 that ignores GIT_NO_LAZY_FETCH.
    sys.simulate(|sys, command| {
        (command.program == Path::new(crate::items::github::CLT_GIT))
            .then(|| crate::init_github::git_before_no_lazy_fetch(sys, command, false))
            .flatten()
    });
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let pending = run(&sys, &ctx, Mode::Apply).unwrap();
    assert_eq!(stage(&pending).outcome, crate::model::Outcome::Blocked);
    assert_eq!(
        Reading::of(stage(&pending)),
        Some(Reading::Waits(Wait::GitReadiness))
    );
    assert_eq!(Wait::GitReadiness.kind(), FailureKind::Transient);
    assert_eq!(
        crate::init_observe::observation(Setting::Repositories, &[stage(&pending)]).standing,
        Standing::Waiting
    );
    let untouched = |calls: &[String]| {
        calls.iter().all(|call| {
            !call.contains(" clone ")
                && !call.contains(" fetch ")
                && !call.contains(" checkout ")
                && !call.contains(" submodule ")
        })
    };
    assert!(untouched(&sys.calls()), "{:?}", sys.calls());
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.contains("--no-lazy-fetch") && call.ends_with(" --version"))
            .count(),
        1
    );

    let prefix = Path::new(&item.user.home).join("Library/Application Support/aterm/pkg");
    let git_store = prefix.join("store/git");
    sys.link(git_store.join("current").to_str().unwrap(), "12");
    sys.stat_of(
        git_store.join("12.ready").to_str().unwrap(),
        stat(false, 0o600),
    );
    sys.stat_of(
        git_store.join("12/bin/git").to_str().unwrap(),
        stat(false, 0o755),
    );
    sys.stat_of(prefix.join("bin/git").to_str().unwrap(), stat(false, 0o755));
    // Re-register read answers for the newly selected executable; policy stays identical.
    repos::tests::script_existing(&sys, &item.user, &parent, "parent");
    required_repository(&sys, &item.user, "parent", &["vendor/library"]);
    existing(&sys, &item.user, &child, "library", PIN);
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    script::alike(&sys, &item.user);
    let ready = run(&sys, &ctx, Mode::Apply).unwrap();
    assert_eq!(
        stage(&ready).outcome,
        crate::model::Outcome::Ok,
        "{ready:?}"
    );
    assert_eq!(
        Reading::of(stage(&ready)),
        Some(Reading::Repositories {
            present: 1,
            count: 1,
            signed_in: true
        })
    );
    assert!(untouched(&sys.calls()), "{:?}", sys.calls());

    sys.file(
        child.join("fixture.rs").to_str().unwrap(),
        b"the person's changes",
    );
    let dirty_child = child.clone();
    sys.simulate(move |_, command| {
        if command.args.iter().any(|arg| arg == "--porcelain=v1") {
            Some(Ran::ok(" M fixture.rs\0"))
        } else if command
            .args
            .iter()
            .any(|arg| arg == dirty_child.as_os_str())
            && command.args.windows(3).any(|args| {
                args[0] == "rev-parse" && args[1] == "--verify" && args[2] == "HEAD^{commit}"
            })
        {
            Some(Ran::ok("2222222222222222222222222222222222222222"))
        } else {
            None
        }
    });
    let personal = run(&sys, &ctx, Mode::Apply).unwrap();
    assert_eq!(
        stage(&personal).outcome,
        crate::model::Outcome::Person,
        "{personal:?}"
    );
    assert_eq!(
        crate::init_observe::observation(Setting::Repositories, &[stage(&personal)]).standing,
        Standing::NeedsYou
    );
    assert_eq!(
        sys.read_file(&child.join("fixture.rs")).unwrap(),
        b"the person's changes"
    );
    assert!(untouched(&sys.calls()), "{:?}", sys.calls());
}

#[test]
fn native_bootstrap_counts_and_repairs_required_gitlinks_on_new_and_existing_parents() {
    for already_present in [false, true] {
        let names = ["parent".to_owned()];
        let (sys, item) = listing(&names, if already_present { &names } else { &[] });
        required_repository(&sys, &item.user, "parent", &["vendor/library"]);
        let parent = Path::new(&item.user.home).join("parent");
        super::super::dependencies::tests::download(
            &sys,
            &item.user,
            &parent,
            "vendor/library",
            u64::from(!already_present),
        );
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert_eq!(
            super::super::automatic_inventory_counts(&sys, &ctx).unwrap(),
            Some(super::InventoryCounts {
                present: 0,
                unborn: 0,
                total: 1
            })
        );
        let rows = run(&sys, &ctx, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Changed,
            "{already_present}: {rows:?}"
        );
        assert_eq!(
            Reading::of(stage(&rows)),
            Some(Reading::Repositories {
                present: 1,
                count: 1,
                signed_in: true
            })
        );
        assert_eq!(
            super::super::automatic_inventory_counts(&sys, &ctx).unwrap(),
            Some(super::InventoryCounts {
                present: 1,
                unborn: 0,
                total: 1
            })
        );
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains(" clone "))
                .count(),
            if already_present { 1 } else { 2 }
        );
        assert!(sys.stat(&parent.join("vendor/library")).is_ok());
        assert!(sys.stat(&parent.join("optional/huge")).is_err());
    }
}

#[test]
fn native_dependency_failure_stays_unfinished_and_the_next_pass_repairs_only_the_child() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let names = ["parent".to_owned()];
    let (sys, item) = listing(&names, &names);
    required_repository(&sys, &item.user, "parent", &["vendor/library"]);
    let parent = Path::new(&item.user.home).join("parent");
    for sequence in 0..2 {
        super::super::dependencies::tests::download(
            &sys,
            &item.user,
            &parent,
            "vendor/library",
            sequence,
        );
    }
    let once = AtomicBool::new(false);
    sys.simulate(move |_, command| {
        (command.display().contains(" clone ") && !once.swap(true, Ordering::SeqCst))
            .then(|| Ran::exit(128, "synthetic transport unavailable"))
    });
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let rows = run(&sys, &ctx, Mode::Apply).unwrap();
    assert_eq!(
        stage(&rows).outcome,
        crate::model::Outcome::Failed,
        "{rows:?}"
    );
    assert_eq!(
        super::super::automatic_inventory_counts(&sys, &ctx).unwrap(),
        Some(super::InventoryCounts {
            present: 0,
            unborn: 0,
            total: 1
        })
    );
    assert!(sys.stat(&parent).is_ok());
    assert!(sys.stat(&parent.join("vendor/library")).is_err());
    let rows = run(&sys, &ctx, Mode::Apply).unwrap();
    assert_eq!(
        stage(&rows).outcome,
        crate::model::Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        super::super::automatic_inventory_counts(&sys, &ctx).unwrap(),
        Some(super::InventoryCounts {
            present: 1,
            unborn: 0,
            total: 1
        })
    );
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.contains(" clone "))
            .count(),
        2
    );
}

#[test]
fn blocked_required_dependency_keeps_its_counts_and_names_preserved_work_not_sign_in() {
    let names = ["parent".to_owned()];
    let (sys, item) = listing(&names, &names);
    required_repository(&sys, &item.user, "parent", &["vendor/library"]);
    let vendor = Path::new(&item.user.home).join("parent/vendor");
    let path = vendor.join("library");
    sys.stat_of(vendor.to_str().unwrap(), stat(true, 0o700));
    sys.file(path.to_str().unwrap(), b"preserved owner work")
        .stat_of(path.to_str().unwrap(), stat(false, 0o600));
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    match item.probe(&sys, &ctx) {
        Probe::Person { step, how } => {
            assert_eq!(step, REPOSITORIES_STEP);
            assert!(
                how.contains("0 of 1 repositories ready") && how.contains("preserved work"),
                "{how}"
            );
            assert!(!how.contains("sign-in"));
            assert!(how.contains("vendor/library"), "{how}");
        }
        other => panic!("{other:?}"),
    }
    let reading = Reading::Repositories {
        present: 0,
        count: 1,
        signed_in: true,
    };
    assert_eq!(item.detail(&sys, &ctx), Some(reading.token()));
    assert_eq!(reading.word().as_deref(), Some("0 of 1"));
    assert_eq!(sys.read_file(&path).unwrap(), b"preserved owner work");
    assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
}

#[test]
fn cached_preserved_dependency_work_is_rechecked_without_a_source_change_or_manual_init() {
    use crate::init::{Holds, OwedClone};
    let names = ["parent".to_owned()];
    let (sys, item) = listing(&names, &names);
    sys.logged_in(&item.user);
    required_repository(&sys, &item.user, "parent", &["vendor/library"]);
    let vendor = Path::new(&item.user.home).join("parent/vendor");
    let path = vendor.join("library");
    sys.stat_of(vendor.to_str().unwrap(), stat(true, 0o700));
    sys.file(path.to_str().unwrap(), b"owner's occupied path")
        .stat_of(path.to_str().unwrap(), stat(false, 0o600));
    super::super::dependencies::tests::download(
        &sys,
        &item.user,
        vendor.parent().unwrap(),
        "vendor/library",
        0,
    );
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let source = super::super::token_read(&sys, &ctx).unwrap().unwrap().mark;
    let state = aterm_tempfile::tempdir().unwrap();
    crate::init::owe_clone(state.path(), &Holds::NONE).unwrap();
    assert!(crate::init::clone_owed(&sys, state.path(), RepositoryWork::Refresh).is_err());
    // The persisted marker is the old untyped format, and must not be treated as an
    // eternal prohibition after the owner corrects only the local dependency path.
    let record = state
        .path()
        .join(aterm_converge::FileJournal::DIR)
        .join("clone-owed");
    let cached = std::fs::read(&record).unwrap();
    assert!(String::from_utf8_lossy(&cached).contains("\nmark "));
    assert!(matches!(
        crate::init::owed_clone_captured(
            &sys,
            state.path(),
            &Holds::NONE,
            InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
            || Ok(true),
            None,
            aterm_secrets::converge::RepositoryWork::Repair,
        )
        .unwrap(),
        OwedClone::Person(_)
    ));
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.contains(" clone ") || call.contains("agh GET"))
    );
    assert_eq!(
        std::fs::read(&record).unwrap(),
        cached,
        "re-probing is read-only"
    );
    sys.remove(&path).unwrap();
    assert_eq!(
        super::super::token_read(&sys, &ctx).unwrap().unwrap().mark,
        source
    );
    assert!(matches!(
        crate::init::owed_clone_captured(
            &sys,
            state.path(),
            &Holds::NONE,
            InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
            || Ok(true),
            None,
            aterm_secrets::converge::RepositoryWork::Repair,
        )
        .unwrap(),
        OwedClone::Due(_)
    ));
    assert!(
        sys.stat(&path).is_err(),
        "a due read still changes no worktree"
    );
    crate::init::clone_owed(&sys, state.path(), RepositoryWork::Refresh).unwrap();
    assert_eq!(
        super::super::token_read(&sys, &ctx).unwrap().unwrap().mark,
        source
    );
    assert_eq!(
        crate::init::owed_clone_captured(
            &sys,
            state.path(),
            &Holds::NONE,
            InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
            || Ok(true),
            None,
            aterm_secrets::converge::RepositoryWork::Repair,
        )
        .unwrap(),
        OwedClone::Verified {
            present: 1,
            total: 1
        }
    );
    assert!(!record.exists());
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.contains(" clone "))
            .count(),
        1
    );
}

#[test]
fn preseeded_checkouts_do_not_verify_an_unusable_preserved_credential_helper() {
    use crate::init::{Holds, OwedClone};
    let names = ["parent".to_owned()];
    let (sys, item) = listing(&names, &names);
    sys.logged_in(&item.user);
    let launcher = aterm_types::installed_command::legacy(Path::new(&item.user.home))[0].clone();
    let preserved = b"preserved older command";
    sys.executable(launcher.to_str().unwrap(), preserved);
    let missing = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let route_missing = Arc::clone(&missing);
    sys.simulate(move |_, command| {
        (command.display().contains("credential fill")
            && route_missing.load(std::sync::atomic::Ordering::SeqCst))
        .then(|| {
            Ran::exit(
                128,
                "fatal: could not read Password: terminal prompts disabled",
            )
        })
    });
    assert!(matches!(
        super::super::vault_git_credentials(&sys).unwrap(),
        crate::sync::GithubRead::NotApplicable(_)
    ));
    let state = aterm_tempfile::tempdir().unwrap();
    let owed = crate::init::owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap();
    assert_eq!(sys.read_file(&launcher).unwrap(), preserved);
    assert!(!sys.calls().iter().any(|call| {
        call.contains(" clone ") || call.contains("agh GET") || call.contains("--unset")
    }));
    assert!(
        matches!(owed, OwedClone::Waiting(_)),
        "matching checkouts cannot verify the unusable preserved route: {owed:?}"
    );
    missing.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        crate::init::owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Verified {
            present: 1,
            total: 1
        },
        "repair of only the preserved route can now verify the same checkouts"
    );
    assert_eq!(sys.read_file(&launcher).unwrap(), preserved);
}

#[test]
fn automatic_readiness_runs_no_helper_for_missing_work_or_ineligible_sources() {
    use crate::init::{Holds, OwedClone};
    use aterm_types::macos_setup::Setting;
    for choice in ["missing", "dependency", "stop", "off", "custom"] {
        let names = ["parent".to_owned()];
        let present = if choice == "missing" {
            &[][..]
        } else {
            &names[..]
        };
        let (sys, item) = listing(&names, present);
        sys.logged_in(&item.user);
        let mut holds = Holds::NONE;
        match choice {
            "dependency" => required_repository(&sys, &item.user, "parent", &["vendor/library"]),
            "stop" => holds.stopped = Some("setup stopped".into()),
            "off" => holds.off.set(Setting::Repositories, true),
            "custom" => scope(&sys, &item.user),
            _ => {}
        }
        sys.simulate(|_, command| {
            assert!(
                !command.display().contains("credential fill"),
                "no route claim is needed"
            );
            None
        });
        let state = aterm_tempfile::tempdir().unwrap();
        let owed = crate::init::owed_clone(&sys, state.path(), &holds, None).unwrap();
        assert!(
            match choice {
                "missing" | "dependency" => matches!(owed, OwedClone::Due(_)),
                "stop" | "off" => matches!(owed, OwedClone::Held(_)),
                "custom" => matches!(owed, OwedClone::Nothing(_)),
                _ => unreachable!(),
            },
            "{choice}: {owed:?}"
        );
    }
}

#[test]
fn captured_readback_refuses_another_login_before_using_its_inputs() {
    let names = ["parent".to_owned()];
    let (mut sys, item) = listing(&names, &names);
    sys.logged_in(&item.user);
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    let other = user("other", 502);
    sys.euid = other.uid;
    sys.logged_in(&other);
    sys.simulate(|_, command| {
        assert!(!command.display().contains("credential fill"));
        None
    });
    let state = aterm_tempfile::tempdir().unwrap();
    let read = crate::init::owed_clone_captured(
        &sys,
        state.path(),
        &crate::init::Holds::NONE,
        inputs,
        || panic!("another account cannot reach the captured vault fence"),
        None,
        aterm_secrets::converge::RepositoryWork::Repair,
    )
    .unwrap();
    assert!(matches!(read, crate::init::OwedClone::Waiting(_)));
}

struct ReadbackFixture {
    paths: aterm_secrets::client::Paths,
    source: std::path::PathBuf,
    source_text: String,
    sys: Fake,
    who: User,
    ctx: Ctx,
    token: std::path::PathBuf,
    _root: aterm_tempfile::TempDir,
}

fn readback_fixture(git: bool) -> ReadbackFixture {
    use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let source = root.path().join("team.secrets");
    let paths = Paths::for_home(&home);
    let inventory = aterm_json::json!({
        "schema":2,"account":"alice","repositories":[repository("alice", "parent", 1)]
    });
    let source_text = format!(
        "gh_access_token={TOKEN}\ngithub-repositories.json={inventory}\nunrelated=before\n"
    );
    aterm_uds::atomic_file::write(&source, source_text.as_bytes(), 0o600).unwrap();
    client::join(
        &paths,
        &JoinOptions {
            source: source.to_string_lossy().into_owned(),
            host: Some("fixture".into()),
            interactive: false,
            wait: std::time::Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
    let fixture = materialized_fixture(root, paths, source, source_text, git, &inventory);
    let parent = home.join("parent");
    fixture
        .sys
        .stat_of(parent.to_str().unwrap(), stat(true, 0o700));
    repos::tests::script_existing(&fixture.sys, &fixture.who, &parent, "parent");
    fixture
}

/// Fake command observations around an already genuinely materialized private vault.
/// Custody and entry production belong to each caller's ordinary client setup.
fn materialized_fixture(
    root: aterm_tempfile::TempDir,
    paths: aterm_secrets::client::Paths,
    source: std::path::PathBuf,
    source_text: String,
    git: bool,
    inventory: &aterm_json::Value,
) -> ReadbackFixture {
    let home = &paths.home;
    let who = User {
        home: home.to_string_lossy().into_owned(),
        ..user("alice", 501)
    };
    let mut sys = Fake::root();
    sys.euid = who.uid;
    sys.logged_in(&who);
    sys.stat_of(home.to_str().unwrap(), stat(true, 0o700));
    if git {
        sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
    }
    let token = home.join(super::super::TOKEN_FILE);
    sys.file(token.to_str().unwrap(), TOKEN.as_bytes())
        .stat_of(token.to_str().unwrap(), stat(false, 0o600));
    metadata(&sys, &who, inventory["repositories"].clone());
    script::launcher(&sys, &who);
    script::lookups_hold(&sys, &who, GITHUB, "alice", TOKEN);
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&who));
    ReadbackFixture {
        paths,
        source,
        source_text,
        sys,
        who,
        ctx,
        token,
        _root: root,
    }
}

/// Ordinary signed creation and publication, before Fake command observations are installed.
fn signed_materialized_fixture(
    inventory: &aterm_json::Value,
    audience: &aterm_secrets::text::Audience,
) -> (ReadbackFixture, u64) {
    use aterm_secrets::client;
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = client::Paths::for_home(&root.path().join("signed-home"));
    let source = root.path().join("team.vault");
    let created = client::create(&paths, source.to_str().unwrap(), None, Some("fixture")).unwrap();
    assert!(created.local.is_none(), "the signed source was installed");
    let admin = client::admin_open(&paths).unwrap();
    let mut document = admin.doc.clone();
    document
        .set_entry(audience, "gh_access_token", TOKEN.as_bytes())
        .unwrap();
    document
        .set_entry(
            audience,
            "github-repositories.json",
            inventory.to_string().as_bytes(),
        )
        .unwrap();
    let published = client::admin_publish(&paths, &admin, &document).unwrap();
    client::sync(&paths, None).unwrap();
    (
        materialized_fixture(root, paths, source, String::new(), true, inventory),
        published.seq,
    )
}

#[test]
fn a_ready_vault_preserves_missing_git_and_hold_dispositions_in_captured_readback() {
    use crate::init::{Holds, OwedClone};
    use aterm_secrets::client;
    use aterm_types::macos_setup::Setting;
    for changed in ["no-git", "held-no-git", "stale-no-git"] {
        let fixture = readback_fixture(false);
        let config = client::config(&fixture.paths).unwrap().unwrap();
        let captured = client::with_materialization_ready_for_entries(
            &fixture.paths,
            &config,
            &super::super::VAULT_INPUT_ENTRIES,
            || {
                InitialRepositoryInputs::capture(&fixture.sys, &fixture.ctx)
                    .map_err(aterm_secrets::Error::new)
            },
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            captured.inputs.local,
            Err(Block {
                wait: Some(Wait::GitReadiness),
                ..
            })
        ));
        let state = aterm_tempfile::tempdir().unwrap();
        let mut holds = Holds::NONE;
        holds
            .off
            .set(Setting::Repositories, changed == "held-no-git");
        let expected = crate::init::owed_clone(&fixture.sys, state.path(), &holds, None).unwrap();
        assert!(matches!(
            expected,
            OwedClone::Waiting(_) | OwedClone::Held(_)
        ));
        if changed == "stale-no-git" {
            aterm_uds::atomic_file::write(
                &fixture.source,
                fixture
                    .source_text
                    .replace(TOKEN, "fixture-rotated-token")
                    .as_bytes(),
                0o600,
            )
            .unwrap();
            client::sync(&fixture.paths, None).unwrap();
        }
        let checked = std::cell::Cell::new(None);
        let actual = crate::init::owed_clone_captured(
            &fixture.sys,
            state.path(),
            &holds,
            captured.inputs,
            || {
                let current = captured
                    .snapshot
                    .is_current(&fixture.paths)
                    .map_err(|why| why.to_string())?;
                checked.set(Some(current));
                Ok(current)
            },
            None,
            aterm_secrets::converge::RepositoryWork::Repair,
        )
        .unwrap();
        assert_eq!(
            checked.get(),
            Some(changed != "stale-no-git"),
            "the full fence always runs"
        );
        if changed == "stale-no-git" {
            assert!(matches!(actual, OwedClone::Waiting(_)));
            assert_ne!(
                actual, expected,
                "a stale vault still replaces the old disposition"
            );
        } else {
            assert_eq!(actual, expected, "a current vault retains {changed}");
        }
        assert!(
            !fixture
                .sys
                .calls()
                .iter()
                .any(|call| call.contains("credential fill") || call.contains(" clone "))
        );
    }

    // A complete inventory can arrive before its credential. Selection, planning and
    // the durable want must not run Git or pretend the account wants no repositories.
    let cold = readback_fixture(true);
    let inventory_only = cold
        .source_text
        .lines()
        .filter(|line| !line.starts_with("gh_access_token="))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    aterm_uds::atomic_file::write(&cold.source, inventory_only.as_bytes(), 0o600).unwrap();
    client::sync(&cold.paths, None).unwrap();
    cold.sys.remove(&cold.token).unwrap();
    cold.sys
        .remove(Path::new(crate::items::github::CLT_GIT))
        .unwrap();
    cold.sys
        .simulate(|_, command| panic!("credential-pending work started {}", command.display()));
    let debt = aterm_tempfile::tempdir().unwrap();
    assert!(matches!(
        crate::init::owed_clone(&cold.sys, debt.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Waiting(_)
    ));
    let pending = NativeRepositories::new(cold.who.clone(), None);
    assert!(matches!(
        pending.probe(&cold.sys, &cold.ctx),
        Probe::Change { .. }
    ));
    run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Plan,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert!(
        aterm_secrets::need::peek(&cold.paths).unwrap().is_empty(),
        "Plan never raises a want"
    );
    // Existing synthetic-policy opt-in tests the consumer lifecycle, not real release.
    aterm_secrets::need::test_alab_policy(&cold.paths);
    run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    let first = aterm_secrets::need::peek(&cold.paths).unwrap();
    assert_eq!(first.len(), 1);
    assert!(!first[0].state.ended());
    let first_id = first[0].id;
    run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(
        aterm_secrets::need::peek(&cold.paths).unwrap()[0].id,
        first_id
    );
    assert!(
        pending
            .apply_guarded(&cold.sys, &cold.ctx, &|| Err("late Stop".into()))
            .is_err()
    );
    assert_eq!(
        aterm_secrets::need::peek(&cold.paths).unwrap()[0].id,
        first_id
    );

    // Withdrawal during the final vault wait must be seen before recording a need.
    // The second caller check holds the final selection read; the third caller
    // check can itself wait. Neither wait preserves the earlier local intent.
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let withdraw = |sys: &Fake, who: &User, caller: &AtomicBool, changed: &str| match changed {
        "stop" => {
            sys.stop_now();
        }
        "profile" => {
            let path = Path::new(&who.home).join(".config/aterm/init.toml");
            let bytes = b"[github]\nenabled=false\n";
            sys.file(path.to_str().unwrap(), bytes).stat_of(
                path.to_str().unwrap(),
                Stat {
                    len: bytes.len() as u64,
                    ..stat(false, 0o600)
                },
            );
        }
        "account" => {
            let mut other = who.clone();
            other.name = "another-account".into();
            sys.logged_in(&other);
        }
        "caller" => {
            caller.store(true, Ordering::SeqCst);
        }
        _ => {}
    };
    for (wait_at, changed) in [
        (2, "none"),
        (2, "stop"),
        (2, "profile"),
        (2, "account"),
        (2, "caller"),
        (3, "none"),
        (3, "stop"),
        (3, "profile"),
        (3, "account"),
    ] {
        let fixture = readback_fixture(true);
        let inventory_only = fixture
            .source_text
            .lines()
            .filter(|line| !line.starts_with("gh_access_token="))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        aterm_uds::atomic_file::write(&fixture.source, inventory_only.as_bytes(), 0o600).unwrap();
        client::sync(&fixture.paths, None).unwrap();
        fixture.sys.remove(&fixture.token).unwrap();
        aterm_secrets::need::test_alab_policy(&fixture.paths);
        let selected = super::super::RepositorySelection::capture(&fixture.sys, &fixture.ctx)
            .unwrap()
            .unwrap();
        assert_eq!(selected.count(), 1);
        let snapshot = client::with_materialization_ready(
            &fixture.paths,
            &client::config(&fixture.paths).unwrap().unwrap(),
            || Ok(()),
        )
        .unwrap()
        .unwrap()
        .snapshot;
        let ledger = fixture.paths.dir.join(aterm_secrets::need::LEDGER);
        let before = std::fs::read(&ledger).ok();
        let sys = Arc::new(fixture.sys);
        let waiting_sys = Arc::clone(&sys);
        let who = fixture.who.clone();
        let writer = Arc::new(Mutex::new(None));
        let waiting_writer = Arc::clone(&writer);
        let withdrawn = Arc::new(AtomicBool::new(false));
        let waiting_withdrawn = Arc::clone(&withdrawn);
        let checks = AtomicUsize::new(0);
        let (raised, waited) = client::test_while_state_lock_held(
            move || {
                withdraw(&waiting_sys, &who, &waiting_withdrawn, changed);
                drop(waiting_writer.lock().unwrap().take());
            },
            || {
                selected.request_credential(
                    sys.as_ref(),
                    &|| {
                        if withdrawn.load(Ordering::SeqCst) {
                            return Err("the caller withdrew repository setup".into());
                        }
                        if checks.fetch_add(1, Ordering::SeqCst) + 1 == wait_at {
                            *writer.lock().unwrap() =
                                Some(client::test_hold_state_lock(&fixture.paths).unwrap());
                            if wait_at == 3
                                && !snapshot
                                    .is_current(&fixture.paths)
                                    .map_err(|why| why.to_string())?
                            {
                                return Err("the caller's captured vault changed".into());
                            }
                        }
                        Ok(())
                    },
                    &|| {
                        if withdrawn.load(Ordering::SeqCst) {
                            Err("the caller withdrew repository setup".into())
                        } else {
                            Ok(())
                        }
                    },
                )
            },
        );
        drop(writer.lock().unwrap().take());
        assert!(
            waited > 0,
            "{changed}: the final admission met the real writer"
        );
        if changed == "none" {
            assert!(raised.unwrap());
            assert_eq!(aterm_secrets::need::peek(&fixture.paths).unwrap().len(), 1);
        } else {
            assert!(raised.is_err(), "{changed}");
            assert_eq!(
                std::fs::read(&ledger).ok(),
                before,
                "{changed}: no withdrawn request is recorded"
            );
        }
    }

    // A different writer can hold the need ledger after every vault check passed.
    // Begin the request while with_ledger holds that real lock, then withdraw only
    // after the request's pre-lock admission has observed the original choice.
    for changed in ["none", "stop", "profile", "account", "caller"] {
        let fixture = readback_fixture(true);
        let inventory_only = fixture
            .source_text
            .lines()
            .filter(|line| !line.starts_with("gh_access_token="))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        aterm_uds::atomic_file::write(&fixture.source, inventory_only.as_bytes(), 0o600).unwrap();
        client::sync(&fixture.paths, None).unwrap();
        fixture.sys.remove(&fixture.token).unwrap();
        aterm_secrets::need::test_alab_policy(&fixture.paths);
        let selected = super::super::RepositorySelection::capture(&fixture.sys, &fixture.ctx)
            .unwrap()
            .unwrap();
        let ledger = fixture.paths.dir.join(aterm_secrets::need::LEDGER);
        let before = std::fs::read(&ledger).ok();
        let withdrawn = AtomicBool::new(false);
        let (go, begin) = std::sync::mpsc::channel();
        let (seen, admission) = std::sync::mpsc::channel();
        let (locked, raised) = std::thread::scope(|scope| {
            let selected = &selected;
            let sys = &fixture.sys;
            let withdrawn = &withdrawn;
            let worker = scope.spawn(move || {
                begin.recv().map_err(|why| why.to_string())?;
                let told = AtomicBool::new(false);
                selected.request_credential(sys, &|| Ok(()), &|| {
                    if withdrawn.load(Ordering::SeqCst) {
                        return Err("the caller withdrew repository setup".into());
                    }
                    if !told.swap(true, Ordering::SeqCst) {
                        seen.send(()).map_err(|why| why.to_string())?;
                    }
                    Ok(())
                })
            });
            let locked = aterm_secrets::need::with_ledger(&fixture.paths, |_, _| {
                go.send(())
                    .map_err(|why| aterm_secrets::Error::new(why.to_string()))?;
                let seen = admission.recv_timeout(std::time::Duration::from_secs(60));
                if seen.is_ok() {
                    withdraw(&fixture.sys, &fixture.who, withdrawn, changed);
                }
                Ok(seen)
            });
            drop(go);
            (locked, worker.join().unwrap())
        });
        locked.unwrap().unwrap();
        if changed == "none" {
            assert!(raised.unwrap());
            assert_eq!(aterm_secrets::need::peek(&fixture.paths).unwrap().len(), 1);
        } else {
            assert!(raised.is_err(), "{changed}");
            assert_eq!(
                std::fs::read(&ledger).ok(),
                before,
                "{changed}: ledger wait cannot retain withdrawn admission"
            );
        }
    }

    // Ordinary materialization supplies the credential before this need was published.
    // Successful paired consumption retires that unnecessary request; it proves neither
    // delivery through the request protocol nor remote GitHub authentication.
    aterm_uds::atomic_file::write(&cold.source, cold.source_text.as_bytes(), 0o600).unwrap();
    client::sync(&cold.paths, None).unwrap();
    cold.sys
        .file(cold.token.to_str().unwrap(), TOKEN.as_bytes())
        .stat_of(cold.token.to_str().unwrap(), stat(false, 0o600));
    cold.sys
        .executable(crate::items::github::CLT_GIT, b"synthetic git");
    cold.sys.simulate(|_, command| {
        let command = command.display();
        assert!(
            !command.contains("ls-remote")
                && !command.contains(" fetch ")
                && !command.contains(" clone "),
            "a local acknowledgment started {command}"
        );
        None
    });
    assert!(
        matches!(
            crate::init::owed_clone(&cold.sys, debt.path(), &Holds::NONE, None).unwrap(),
            OwedClone::Due(_)
        ),
        "automatic already-present work must still own the local credential acknowledgment"
    );
    let supplied = run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(stage(&supplied).outcome, crate::model::Outcome::Changed);
    let consumed_need = aterm_secrets::need::peek(&cold.paths).unwrap().remove(0);
    assert_eq!(consumed_need.state, aterm_secrets::need::State::Retired);
    assert_eq!(
        consumed_need.retired.as_ref().unwrap().reason,
        aterm_secrets::need::Reason::NotNeeded
    );
    let confirmed = run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Plan,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(stage(&confirmed).outcome, crate::model::Outcome::Ok);
    aterm_uds::atomic_file::write(&cold.source, inventory_only.as_bytes(), 0o600).unwrap();
    client::sync(&cold.paths, None).unwrap();
    cold.sys.remove(&cold.token).unwrap();
    cold.sys
        .remove(Path::new(crate::items::github::CLT_GIT))
        .unwrap();
    cold.sys
        .simulate(|_, command| panic!("missing credential started {}", command.display()));
    run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    let rearmed = aterm_secrets::need::peek(&cold.paths).unwrap();
    assert_ne!(rearmed[0].id, first_id);
    let rearmed_id = rearmed[0].id;
    run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(
        aterm_secrets::need::peek(&cold.paths).unwrap()[0].id,
        rearmed_id
    );
    let stale = NativeRepositories::new(cold.who.clone(), None);
    assert!(matches!(
        stale.probe(&cold.sys, &cold.ctx),
        Probe::Change { .. }
    ));
    let empty_only =
        "github-repositories.json={\"schema\":2,\"account\":\"alice\",\"repositories\":[]}\n";
    aterm_uds::atomic_file::write(&cold.source, empty_only.as_bytes(), 0o600).unwrap();
    client::sync(&cold.paths, None).unwrap();
    metadata(&cold.sys, &cold.who, aterm_json::json!([]));
    assert!(
        stale
            .apply_guarded(&cold.sys, &cold.ctx, &|| Ok(()))
            .is_err(),
        "old selection cannot request for a new inventory"
    );
    let ledger_file = cold.paths.dir.join(aterm_secrets::need::LEDGER);
    let pending_bytes = std::fs::read(&ledger_file).unwrap();
    assert!(matches!(
        crate::init::owed_clone(&cold.sys, debt.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Due(_)
    ));
    let mut stopped = Holds::NONE;
    stopped.stopped = Some("you stopped setup in System State".into());
    assert!(matches!(
        crate::init::owed_clone(&cold.sys, debt.path(), &stopped, None).unwrap(),
        OwedClone::Waiting(_) | OwedClone::Held(_)
    ));
    run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Plan,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(std::fs::read(&ledger_file).unwrap(), pending_bytes);
    let withdrawal = super::super::RepositoryCredentialWithdrawal::capture(&cold.sys, &cold.ctx)
        .unwrap()
        .unwrap();
    let withheld = run_withdrawal(
        &cold.sys,
        &cold.ctx,
        Mode::Apply,
        withdrawal.clone(),
        Box::new(|| Ok(false)),
    )
    .unwrap();
    assert_eq!(stage(&withheld).outcome, crate::model::Outcome::Blocked);
    assert_eq!(std::fs::read(&ledger_file).unwrap(), pending_bytes);
    let local = NativeRepositories {
        local_withdrawal: Some(withdrawal),
        ..NativeRepositories::new(cold.who.clone(), None)
    };
    assert!(matches!(
        local.probe(&cold.sys, &cold.ctx),
        Probe::Change { .. }
    ));
    assert!(
        local
            .apply_guarded(&cold.sys, &cold.ctx, &|| Err("late Stop".into()))
            .is_err()
    );
    assert_eq!(std::fs::read(&ledger_file).unwrap(), pending_bytes);
    aterm_uds::atomic_file::write(&cold.source, inventory_only.as_bytes(), 0o600).unwrap();
    client::sync(&cold.paths, None).unwrap();
    metadata(
        &cold.sys,
        &cold.who,
        aterm_json::json!([repository("alice", "parent", 1)]),
    );
    assert!(
        local
            .apply_guarded(&cold.sys, &cold.ctx, &|| Ok(()))
            .is_err(),
        "an empty selection cannot retire intent after the inventory becomes nonempty"
    );
    assert_eq!(std::fs::read(&ledger_file).unwrap(), pending_bytes);
    aterm_uds::atomic_file::write(&cold.source, empty_only.as_bytes(), 0o600).unwrap();
    client::sync(&cold.paths, None).unwrap();
    metadata(&cold.sys, &cold.who, aterm_json::json!([]));
    let empty_rows = run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(
        stage(&empty_rows).outcome,
        crate::model::Outcome::Changed,
        "{empty_rows:?}"
    );
    assert!(matches!(
        crate::init::owed_clone(&cold.sys, debt.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Nothing(_)
    ));
    let after_empty = aterm_secrets::need::peek(&cold.paths).unwrap();
    assert_eq!(after_empty[0].id, rearmed_id);
    assert_eq!(after_empty[0].state, aterm_secrets::need::State::Retired);
    assert_eq!(
        after_empty[0].retired.as_ref().unwrap().reason,
        aterm_secrets::need::Reason::NotNeeded
    );
    // Re-enabling the same nonempty intent raises a fresh request. Unknown inventory
    // cannot withdraw it; explicit opt-out can, without a credential or native command.
    aterm_uds::atomic_file::write(&cold.source, inventory_only.as_bytes(), 0o600).unwrap();
    client::sync(&cold.paths, None).unwrap();
    metadata(
        &cold.sys,
        &cold.who,
        aterm_json::json!([repository("alice", "parent", 1)]),
    );
    run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    let reenabled = aterm_secrets::need::peek(&cold.paths).unwrap().remove(0);
    assert_ne!(reenabled.id, rearmed_id);
    assert!(!reenabled.state.ended());
    let reenabled_bytes = std::fs::read(&ledger_file).unwrap();
    let inventory_file = super::super::inventory::file(&cold.who.home);
    cold.sys
        .file(inventory_file.to_str().unwrap(), b"not-json")
        .stat_of(
            inventory_file.to_str().unwrap(),
            Stat {
                len: 8,
                ..stat(false, 0o600)
            },
        );
    assert!(super::super::RepositoryCredentialWithdrawal::capture(&cold.sys, &cold.ctx).is_err());
    let unknown_rows = run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Plan,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(stage(&unknown_rows).outcome, crate::model::Outcome::Blocked);
    assert!(
        super::super::items_for_execution(&cold.sys, &cold.ctx, None, RepositoryWork::Repair, None)
            .is_ok(),
        "unknown cleanup intent must not abort construction of other setup groups"
    );
    assert_eq!(std::fs::read(&ledger_file).unwrap(), reenabled_bytes);
    metadata(
        &cold.sys,
        &cold.who,
        aterm_json::json!([repository("alice", "parent", 1)]),
    );
    let allow_withdrawal = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let allowed = Arc::clone(&allow_withdrawal);
    let guarded_items = super::super::items_for_execution_with_withdrawal_guard(
        &cold.sys,
        &cold.ctx,
        None,
        RepositoryWork::Repair,
        None,
        Some(Box::new(move || {
            Ok(allowed.load(std::sync::atomic::Ordering::SeqCst))
        })),
    )
    .unwrap();
    let guarded_native = guarded_items
        .iter()
        .find(|item| item.id().starts_with("init.github.native@"))
        .unwrap();
    let profile = Path::new(&cold.who.home).join(".config/aterm/init.toml");
    let disabled = b"[github]\nenabled=false\n";
    cold.sys.file(profile.to_str().unwrap(), disabled).stat_of(
        profile.to_str().unwrap(),
        Stat {
            len: disabled.len() as u64,
            ..stat(false, 0o600)
        },
    );
    assert!(matches!(
        guarded_native.probe(&cold.sys, &cold.ctx),
        Probe::Change { .. }
    ));
    allow_withdrawal.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        guarded_native
            .apply_guarded(&cold.sys, &cold.ctx, &|| Ok(()))
            .is_err()
    );
    assert_eq!(std::fs::read(&ledger_file).unwrap(), reenabled_bytes);
    let rows = run_with(
        &cold.sys,
        &cold.ctx,
        Mode::Plan,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "local withdrawal does not add a keychain stage"
    );
    assert_eq!(std::fs::read(&ledger_file).unwrap(), reenabled_bytes);
    let changed_choice = NativeRepositories::new(cold.who.clone(), None);
    assert!(matches!(
        changed_choice.probe(&cold.sys, &cold.ctx),
        Probe::Change { .. }
    ));
    cold.sys.remove(&profile).unwrap();
    assert!(
        changed_choice
            .apply_guarded(&cold.sys, &cold.ctx, &|| Ok(()))
            .is_err()
    );
    assert_eq!(std::fs::read(&ledger_file).unwrap(), reenabled_bytes);
    cold.sys.file(profile.to_str().unwrap(), disabled).stat_of(
        profile.to_str().unwrap(),
        Stat {
            len: disabled.len() as u64,
            ..stat(false, 0o600)
        },
    );
    assert_eq!(
        crate::init::owed_clone_control(&cold.sys, debt.path(), &Holds::NONE).unwrap(),
        None,
        "explicit opt-out still admits its current local request retirement"
    );
    let mut held = Holds::NONE;
    held.off.set(Setting::Repositories, true);
    assert!(
        crate::init::owed_clone_control(&cold.sys, debt.path(), &held)
            .unwrap()
            .is_some(),
        "a current hold still fences that local retirement"
    );
    assert_eq!(std::fs::read(&ledger_file).unwrap(), reenabled_bytes);
    assert!(
        crate::init::OwedCloneIntent::capture(&cold.sys, debt.path(), &held).is_err(),
        "a named clone does not admit credential retirement over a held group"
    );
    let intent = Arc::new(
        crate::init::OwedCloneIntent::capture(&cold.sys, debt.path(), &Holds::NONE).unwrap(),
    );
    assert!(
        intent
            .current(&cold.sys, debt.path(), &Holds::NONE)
            .unwrap()
    );
    assert!(!intent.current(&cold.sys, debt.path(), &held).unwrap());
    let mut stopped = Holds::NONE;
    stopped.stopped = Some("a later Stop".into());
    assert!(!intent.current(&cold.sys, debt.path(), &stopped).unwrap());
    let mut newer = Holds::NONE;
    newer.newer = true;
    assert!(!intent.current(&cold.sys, debt.path(), &newer).unwrap());
    let withdrawal = super::super::RepositoryCredentialWithdrawal::capture(&cold.sys, &cold.ctx)
        .unwrap()
        .unwrap();
    let sys = Arc::new(cold.sys);
    let current_sys = Arc::clone(&sys);
    let current_intent = Arc::clone(&intent);
    let current_state = debt.path().to_path_buf();
    let off_rows = run_withdrawal(
        sys.as_ref(),
        &cold.ctx,
        Mode::Apply,
        withdrawal,
        Box::new(move || {
            current_intent.current(current_sys.as_ref(), &current_state, &Holds::NONE)
        }),
    )
    .unwrap();
    assert_eq!(off_rows.len(), 1);
    assert_eq!(stage(&off_rows).outcome, crate::model::Outcome::Changed);
    assert!(
        intent
            .current(sys.as_ref(), debt.path(), &Holds::NONE)
            .unwrap(),
        "the captured cleanup remains current through its own completed retirement"
    );
    assert!(!intent.current(sys.as_ref(), debt.path(), &held).unwrap());
    assert!(!intent.current(sys.as_ref(), debt.path(), &stopped).unwrap());
    assert!(!intent.current(sys.as_ref(), debt.path(), &newer).unwrap());
    assert!(matches!(
        changed_choice.probe(sys.as_ref(), &cold.ctx),
        Probe::Ok { .. }
    ));
    let withdrawn = aterm_secrets::need::peek(&cold.paths).unwrap().remove(0);
    assert_eq!(withdrawn.id, reenabled.id);
    assert_eq!(withdrawn.state, aterm_secrets::need::State::Retired);
    assert!(
        crate::init::owed_clone_control(sys.as_ref(), debt.path(), &Holds::NONE)
            .unwrap()
            .is_some(),
        "without pending retirement the explicit Off control still holds"
    );
    assert_eq!(
        withdrawn.retired.as_ref().unwrap().reason,
        aterm_secrets::need::Reason::NotNeeded
    );
    let config_file = cold.paths.dir.join("source.toml");
    let config_bytes = std::fs::read(&config_file).unwrap();
    aterm_uds::atomic_file::write(&config_file, b"not toml [", 0o600).unwrap();
    assert!(client::config(&cold.paths).is_err());
    assert!(
        super::super::items_for_execution(
            sys.as_ref(),
            &cold.ctx,
            None,
            RepositoryWork::Repair,
            None
        )
        .unwrap()
        .is_empty(),
        "disabled setup without an active request needs no vault-source admission"
    );
    aterm_uds::atomic_file::write(&config_file, &config_bytes, 0o600).unwrap();
    sys.remove(&profile).unwrap();
    assert!(matches!(
        crate::init::owed_clone(sys.as_ref(), debt.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Waiting(_)
    ));

    // The plain-file fixture has no release policy by default; a credential intent
    // records the existing policy refusal rather than claiming automatic delivery.
    let forbidden = readback_fixture(false);
    let without_credential = forbidden
        .source_text
        .lines()
        .filter(|line| !line.starts_with("gh_access_token="))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    aterm_uds::atomic_file::write(&forbidden.source, without_credential.as_bytes(), 0o600).unwrap();
    client::sync(&forbidden.paths, None).unwrap();
    forbidden.sys.remove(&forbidden.token).unwrap();
    let denied = run_with(
        &forbidden.sys,
        &forbidden.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(stage(&denied).outcome, crate::model::Outcome::Failed);
    let refused = aterm_secrets::need::peek(&forbidden.paths).unwrap();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].state, aterm_secrets::need::State::Retired);

    // A fully materialized empty selection owes no repository or sign-in
    // work, including on an account where Git and its helper are absent.
    let fixture = readback_fixture(false);
    let empty = aterm_json::json!({"schema":2,"account":"alice","repositories":[]});
    let source_text =
        format!("gh_access_token={TOKEN}\ngithub-repositories.json={empty}\nunrelated=before\n");
    aterm_uds::atomic_file::write(&fixture.source, source_text.as_bytes(), 0o600).unwrap();
    client::sync(&fixture.paths, None).unwrap();
    metadata(&fixture.sys, &fixture.who, aterm_json::json!([]));
    fixture
        .sys
        .remove(&aterm_types::installed_command::legacy(Path::new(&fixture.who.home))[0])
        .unwrap();
    fixture.sys.simulate(|_, command| {
        panic!(
            "an empty repository selection started {}",
            command.display()
        )
    });
    let config = client::config(&fixture.paths).unwrap().unwrap();
    let captured = client::with_materialization_ready_for_entries(
        &fixture.paths,
        &config,
        &super::super::VAULT_INPUT_ENTRIES,
        || {
            InitialRepositoryInputs::capture(&fixture.sys, &fixture.ctx)
                .map_err(aterm_secrets::Error::new)
        },
    )
    .unwrap()
    .unwrap();
    let state = aterm_tempfile::tempdir().unwrap();
    for legacy in [false, true] {
        if legacy {
            crate::init::owe_clone(state.path(), &Holds::NONE).unwrap();
        }
        let checked = std::cell::Cell::new(false);
        let read = crate::init::owed_clone_captured(
            &fixture.sys,
            state.path(),
            &Holds::NONE,
            captured.inputs.clone(),
            || {
                checked.set(true);
                captured
                    .snapshot
                    .is_current(&fixture.paths)
                    .map_err(|why| why.to_string())
            },
            None,
            aterm_secrets::converge::RepositoryWork::Repair,
        )
        .unwrap();
        assert!(
            checked.get(),
            "empty work still requires the full vault fence"
        );
        assert!(matches!(read, OwedClone::Nothing(_)), "{legacy}: {read:?}");
        assert!(matches!(
            crate::init::owed_clone(&fixture.sys, state.path(), &Holds::NONE, None).unwrap(),
            OwedClone::Nothing(_)
        ));
    }
    for mode in [Mode::Plan, Mode::Apply] {
        let paths = fixture.paths.clone();
        let snapshot = captured.snapshot.clone();
        let rows = run_captured(
            &fixture.sys,
            &fixture.ctx,
            mode,
            RepositoryWork::Repair,
            captured.inputs.clone(),
            Box::new(move || snapshot.is_current(&paths).map_err(|why| why.to_string())),
        )
        .unwrap();
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::NotApplicable);
        assert!(Reading::of(stage(&rows)).is_none());
    }
    let mut holds = Holds::NONE;
    holds.off.set(Setting::Repositories, true);
    assert!(matches!(
        crate::init::owed_clone_captured(
            &fixture.sys,
            state.path(),
            &holds,
            captured.inputs.clone(),
            || Ok(true),
            None,
            aterm_secrets::converge::RepositoryWork::Repair,
        )
        .unwrap(),
        OwedClone::Held(_)
    ));
    aterm_uds::atomic_file::write(
        &fixture.source,
        source_text
            .replace(TOKEN, "fixture-rotated-token")
            .as_bytes(),
        0o600,
    )
    .unwrap();
    client::sync(&fixture.paths, None).unwrap();
    assert!(matches!(
        crate::init::owed_clone_captured(
            &fixture.sys,
            state.path(),
            &Holds::NONE,
            captured.inputs,
            || {
                captured
                    .snapshot
                    .is_current(&fixture.paths)
                    .map_err(|why| why.to_string())
            },
            None,
            aterm_secrets::converge::RepositoryWork::Repair,
        )
        .unwrap(),
        OwedClone::Waiting(_)
    ));

    // A present credential that the provider definitively rejects is different from
    // missing materialization. Exercise the actual private clone failure before the
    // canonical need is inspected; no live provider or credential is involved.
    let repositories = aterm_json::json!([
        repository("alice", "private-missing", 1),
        repository("alice", "also-missing", 2),
    ]);
    let inventory = aterm_json::json!({
        "schema":2,"account":"alice","repositories":repositories
    });
    let audience = aterm_secrets::text::Audience::Hosts(vec!["fixture".into()]);
    let (rejected, published_seq) = signed_materialized_fixture(&inventory, &audience);
    for name in ["private-missing", "also-missing"] {
        let endpoint = format!("https://github.com/alice/{name}.git");
        rejected.sys.on(
            &crate::checkout::git(&rejected.sys, &rejected.who, None)
                .args(["ls-remote", "--get-url", "--", &endpoint])
                .display(),
            Ran::ok(&endpoint),
        );
    }
    aterm_secrets::need::test_alab_policy(&rejected.paths);
    let sequence = aterm_secrets::need::facts(&rejected.paths, 1)
        .unwrap()
        .sight
        .unwrap()
        .seq;
    assert_eq!(
        sequence, published_seq,
        "the paired token has an actual signed version"
    );
    rejected.sys.simulate(|_, command| {
        command
            .display()
            .contains("ls-remote --symref --")
            .then(|| Ran::exit(128, "synthetic remote failure; no private output"))
    });
    for status in [401, 401, 401, 403, 429] {
        rejected.sys.on_github_api(
            &rejected.who,
            "/user",
            Ok(GithubResponse {
                status,
                headers: Vec::new(),
                body: br#"{"login":"alice"}"#.to_vec(),
            }),
        );
    }
    rejected.sys.fail_github_api(
        &rejected.who,
        "/user",
        crate::port::GithubFailure::Unreached,
    );
    for status in [200, 200, 403, 429] {
        rejected.sys.on_github_api(
            &rejected.who,
            "/user",
            Ok(GithubResponse {
                status,
                headers: Vec::new(),
                body: br#"{"login":"alice"}"#.to_vec(),
            }),
        );
    }
    rejected.sys.fail_github_api(
        &rejected.who,
        "/user",
        crate::port::GithubFailure::Unreached,
    );
    let want = aterm_secrets::need::Want::Gh(aterm_secrets::need::GhProfile::Dev);
    let purpose = aterm_secrets::need::Purpose::AghDev;
    let pending = aterm_secrets::need::raise(&rejected.paths, want.clone(), purpose).unwrap();
    assert!(!pending.got && pending.refused.is_none() && !pending.state.ended());
    let failed = run_with(
        &rejected.sys,
        &rejected.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(
        stage(&failed).outcome,
        crate::model::Outcome::Failed,
        "{failed:#?}"
    );
    assert_eq!(
        rejected
            .sys
            .calls()
            .iter()
            .filter(|call| call.contains("ls-remote --symref --"))
            .count(),
        2,
        "both failed workers complete before the one auth classification"
    );
    let requests = aterm_secrets::need::peek(&rejected.paths).unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the rejected present credential needs recovery"
    );
    assert_eq!(
        requests[0]
            .refused
            .as_ref()
            .and_then(|floor| floor.sequence()),
        Some(sequence)
    );
    assert_ne!(
        requests[0].id, pending.id,
        "a posted request body cannot gain a new floor in place"
    );
    assert!(!requests[0].state.ended());
    let config = client::config(&rejected.paths).unwrap().unwrap();
    let paired = || {
        client::with_materialization_ready_for_entries(
            &rejected.paths,
            &config,
            &super::super::VAULT_INPUT_ENTRIES,
            || Ok(()),
        )
        .unwrap()
        .unwrap()
        .snapshot
    };
    let ledger = rejected.paths.dir.join(aterm_secrets::need::LEDGER);
    let refused_bytes = std::fs::read(&ledger).unwrap();
    assert!(
        aterm_secrets::need::acknowledge_from_snapshot(
            &rejected.paths,
            &pending,
            &paired(),
            || Ok(()),
        )
        .is_err(),
        "a local acceptance observed before None became a refusal cannot retire it"
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), refused_bytes);
    assert_eq!(
        rejected
            .sys
            .calls()
            .iter()
            .filter(|call| call.contains("agh GET /user"))
            .count(),
        1
    );
    let run_failed = || {
        let rows = run_with(
            &rejected.sys,
            &rejected.ctx,
            Mode::Apply,
            RepositoryWork::Repair,
            None,
        )
        .unwrap();
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Failed);
    };
    run_failed();
    assert_eq!(
        aterm_secrets::need::peek(&rejected.paths).unwrap()[0].id,
        requests[0].id,
        "a repeated refusal at the same version keeps its ID"
    );

    let admin = client::admin_open(&rejected.paths).unwrap();
    let mut later = admin.doc.clone();
    later.set_entry(&audience, "unrelated", b"later").unwrap();
    let later = client::admin_publish(&rejected.paths, &admin, &later).unwrap();
    client::sync(&rejected.paths, None).unwrap();
    assert!(later.seq > sequence);
    run_failed();
    let advanced = aterm_secrets::need::peek(&rejected.paths)
        .unwrap()
        .remove(0);
    assert_ne!(
        advanced.id, requests[0].id,
        "an active undelivered request needs a fresh body for its newer floor"
    );
    assert_eq!(
        advanced.refused.as_ref().and_then(|floor| floor.sequence()),
        Some(later.seq)
    );
    run_failed();
    assert_eq!(
        aterm_secrets::need::peek(&rejected.paths).unwrap()[0].id,
        advanced.id
    );
    let lower =
        aterm_secrets::need::raise_refused(&rejected.paths, want.clone(), purpose, sequence)
            .unwrap();
    assert_eq!(
        (lower.id, lower.refused),
        (advanced.id, advanced.refused.clone())
    );

    let api_calls = || {
        rejected
            .sys
            .calls()
            .iter()
            .filter(|call| call.contains("agh GET /user"))
            .count()
    };
    assert_eq!(
        api_calls(),
        2,
        "the same authenticated refused version does not repeat classification"
    );
    let paths: Vec<_> = ["private-missing", "also-missing"]
        .map(|name| Path::new(&rejected.who.home).join(name))
        .into();
    for path in &paths {
        rejected
            .sys
            .stat_of(path.to_str().unwrap(), stat(true, 0o700));
        repos::tests::script_existing(
            &rejected.sys,
            &rejected.who,
            path,
            path.file_name().unwrap().to_str().unwrap(),
        );
    }
    run_failed();
    assert_eq!(
        api_calls(),
        2,
        "local-only ack must not become a healthy authentication probe"
    );
    assert_eq!(
        aterm_secrets::need::peek(&rejected.paths).unwrap()[0].id,
        advanced.id,
        "helper equality cannot erase this version's definitive refusal"
    );
    let state = aterm_tempfile::tempdir().unwrap();
    assert!(matches!(
        crate::init::owed_clone(&rejected.sys, state.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Due(_)
    ));

    // A newer unrelated version merely permits recovery. With all worktrees present,
    // helper equality still cannot erase the provider's explicit refusal.
    let admin = client::admin_open(&rejected.paths).unwrap();
    let mut eligible = admin.doc.clone();
    eligible
        .set_entry(&audience, "unrelated", b"eligible")
        .unwrap();
    let eligible = client::admin_publish(&rejected.paths, &admin, &eligible).unwrap();
    client::sync(&rejected.paths, None).unwrap();
    run_failed();
    let refused_again = aterm_secrets::need::peek(&rejected.paths)
        .unwrap()
        .remove(0);
    assert_ne!(refused_again.id, advanced.id);
    assert_eq!(
        refused_again
            .refused
            .as_ref()
            .and_then(|floor| floor.sequence()),
        Some(eligible.seq)
    );
    assert_eq!(api_calls(), 3);
    run_failed();
    assert_eq!(
        api_calls(),
        3,
        "the same refused version does not repeat recovery auth"
    );
    let admin = client::admin_open(&rejected.paths).unwrap();
    let mut eligible = admin.doc.clone();
    eligible
        .set_entry(&audience, "unrelated", b"eligible-again")
        .unwrap();
    client::admin_publish(&rejected.paths, &admin, &eligible).unwrap();
    client::sync(&rejected.paths, None).unwrap();
    for _ in [403, 429, 0] {
        run_failed();
        assert_eq!(
            aterm_secrets::need::peek(&rejected.paths).unwrap()[0].id,
            refused_again.id,
            "an unverified response cannot acknowledge or reissue"
        );
    }
    // Model a provider response arriving after another failed consumer has already
    // advanced the durable refusal. Acceptance belongs to the observed request only.
    let receipt = paired();
    let concurrent = aterm_secrets::need::raise_refused_from_snapshot(
        &rejected.paths,
        want.clone(),
        purpose,
        &receipt,
        || Ok(()),
    )
    .unwrap();
    assert_ne!(concurrent.id, refused_again.id);
    assert_eq!(
        concurrent
            .refused
            .as_ref()
            .and_then(|floor| floor.sequence()),
        Some(receipt.sequence())
    );
    let refused_bytes = std::fs::read(&ledger).unwrap();
    assert!(
        aterm_secrets::need::acknowledge_from_snapshot(
            &rejected.paths,
            &refused_again,
            &receipt,
            || Ok(()),
        )
        .is_err(),
        "stale acceptance cannot retire a concurrently replaced request"
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), refused_bytes);
    let admin = client::admin_open(&rejected.paths).unwrap();
    let mut eligible = admin.doc.clone();
    eligible
        .set_entry(&audience, "unrelated", b"after-concurrent-refusal")
        .unwrap();
    client::admin_publish(&rejected.paths, &admin, &eligible).unwrap();
    client::sync(&rejected.paths, None).unwrap();
    let accepted = run_with(
        &rejected.sys,
        &rejected.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(stage(&accepted).outcome, crate::model::Outcome::Changed);
    assert!(
        aterm_secrets::need::peek(&rejected.paths).unwrap()[0]
            .state
            .ended()
    );
    assert_eq!(
        api_calls(),
        7,
        "only the parsed provider success accepts recovery"
    );

    // Successful recovery leaves a stable ended ledger row: none of these
    // ordinary remote outcomes may turn it into another credential request.
    for path in &paths {
        rejected.sys.remove(path).unwrap();
    }
    let ledger = rejected.paths.dir.join(aterm_secrets::need::LEDGER);
    let before = std::fs::read(&ledger).unwrap();
    for _ in [200, 403, 429, 0] {
        run_failed();
        assert_eq!(
            std::fs::read(&ledger).unwrap(),
            before,
            "non-401 responses and an unreached provider do not rearm"
        );
    }
    assert_eq!(api_calls(), 11);

    for path in &paths {
        rejected
            .sys
            .stat_of(path.to_str().unwrap(), stat(true, 0o700));
    }
    let ready = run_with(
        &rejected.sys,
        &rejected.ctx,
        Mode::Apply,
        RepositoryWork::Repair,
        None,
    )
    .unwrap();
    assert_eq!(stage(&ready).outcome, crate::model::Outcome::Ok);
    assert_eq!(
        api_calls(),
        11,
        "healthy preseeded work stays entirely local"
    );

    for path in &paths {
        rejected.sys.remove(path).unwrap();
    }
    rejected.sys.simulate(|_, command| {
        command
            .display()
            .contains("ls-remote --get-url --")
            .then(|| Ran::exit(1, "synthetic local route inspection failure"))
    });
    run_failed();
    assert_eq!(
        api_calls(),
        11,
        "local failures never classify provider authentication"
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), before);
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Arc::clone(&stopped);
    rejected.sys.simulate(move |_, command| {
        command
            .display()
            .contains("ls-remote --symref --")
            .then(|| {
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
                Ran::exit(128, "synthetic failed network step")
            })
    });
    let mut item = NativeRepositories::new(rejected.who.clone(), None);
    item.work = RepositoryWork::Repair;
    assert!(matches!(
        item.probe(&rejected.sys, &rejected.ctx),
        Probe::Change { .. }
    ));
    assert!(
        item.apply_guarded(&rejected.sys, &rejected.ctx, &|| {
            if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                Err("late Stop".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert!(stopped.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(api_calls(), 11, "Stop before recovery starts no auth read");
    assert_eq!(std::fs::read(&ledger).unwrap(), before);

    // The exact-version registration must reject a later unrelated publication even
    // though the scoped token/inventory currency intentionally remains valid.
    let config = client::config(&rejected.paths).unwrap().unwrap();
    let captured = client::with_materialization_ready_for_entries(
        &rejected.paths,
        &config,
        &super::super::VAULT_INPUT_ENTRIES,
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    let admin = client::admin_open(&rejected.paths).unwrap();
    let mut changed = admin.doc.clone();
    changed.set_entry(&audience, "unrelated", b"newer").unwrap();
    client::admin_publish(&rejected.paths, &admin, &changed).unwrap();
    client::sync(&rejected.paths, None).unwrap();
    assert!(captured.snapshot.is_current(&rejected.paths).unwrap());
    assert!(
        aterm_secrets::need::raise_refused_from_snapshot(
            &rejected.paths,
            want.clone(),
            purpose,
            &captured.snapshot,
            || Ok(())
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), before);
    let captured = client::with_materialization_ready_for_entries(
        &rejected.paths,
        &config,
        &super::super::VAULT_INPUT_ENTRIES,
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    assert!(
        aterm_secrets::need::raise_refused_from_snapshot(
            &rejected.paths,
            want,
            purpose,
            &captured.snapshot,
            || Err(aterm_secrets::Error::new("late Stop"))
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), before);
    let whole = client::with_materialization_ready(&rejected.paths, &config, || Ok(()))
        .unwrap()
        .unwrap();
    assert!(
        aterm_secrets::need::raise_refused_from_snapshot(
            &rejected.paths,
            aterm_secrets::need::Want::Notary,
            aterm_secrets::need::Purpose::ReleaseNotary,
            &whole.snapshot,
            || Ok(())
        )
        .is_err(),
        "a whole snapshot without the requested credential is no receipt"
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), before);

    // A genuinely observed nonzero legacy floor still blocks both retry paths
    // until its signed source advances; it is not the unknown log-only zero case.
    let want = aterm_secrets::need::Want::Gh(aterm_secrets::need::GhProfile::Dev);
    let (legacy, scalar) = signed_materialized_fixture(&inventory, &audience);
    assert!(scalar > 0);
    aterm_secrets::need::test_alab_policy(&legacy.paths);
    let raised =
        aterm_secrets::need::raise_refused(&legacy.paths, want.clone(), purpose, scalar).unwrap();
    assert!(!raised.state.ended());
    assert!(matches!(raised.refused,
        Some(aterm_secrets::need::RefusalClaim::LegacySequence(sequence)) if sequence == scalar));
    let record = legacy.paths.dir.join(aterm_secrets::need::LEDGER);
    let legacy_bytes = std::fs::read(&record).unwrap();
    for name in ["private-missing", "also-missing"] {
        let endpoint = format!("https://github.com/alice/{name}.git");
        legacy.sys.on(
            &crate::checkout::git(&legacy.sys, &legacy.who, None)
                .args(["ls-remote", "--get-url", "--", &endpoint])
                .display(),
            Ran::ok(&endpoint),
        );
    }
    legacy.sys.simulate(|_, command| {
        command
            .display()
            .contains("ls-remote --symref --")
            .then(|| Ran::exit(128, "synthetic legacy remote failure"))
    });
    let run_legacy = || {
        let rows = run_with(
            &legacy.sys,
            &legacy.ctx,
            Mode::Apply,
            RepositoryWork::Repair,
            None,
        )
        .unwrap();
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Failed);
        assert!(
            !legacy
                .sys
                .calls()
                .iter()
                .any(|call| call.contains("agh GET /user"))
        );
        assert_eq!(std::fs::read(&record).unwrap(), legacy_bytes);
    };
    run_legacy();
    for name in ["private-missing", "also-missing"] {
        let path = Path::new(&legacy.who.home).join(name);
        legacy
            .sys
            .stat_of(path.to_str().unwrap(), stat(true, 0o700));
        repos::tests::script_existing(&legacy.sys, &legacy.who, &path, name);
    }
    run_legacy();
    assert_eq!(std::fs::read(&ledger).unwrap(), before);

    // Exercise the actual Native consumer on a supported log-only recipient,
    // established by ordinary URL creation, genesis and the member's signed handout.
    struct HeldServer(Option<aterm_secrets::server::Running>);
    impl HeldServer {
        fn stop(&mut self) {
            if let Some(server) = self.0.take() {
                server.stop();
            }
        }
    }
    impl Drop for HeldServer {
        fn drop(&mut self) {
            self.stop();
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let listen = listener.local_addr().unwrap();
    let store = root.path().join("served");
    let mut server = HeldServer(Some(
        aterm_secrets::server::start(
            listener,
            &aterm_secrets::server::Options {
                listen,
                store: store.clone(),
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(10),
                keep_awake: false,
            },
        )
        .unwrap(),
    ));
    let admin_paths = client::Paths::for_home(&root.path().join("admin"));
    client::create(
        &admin_paths,
        &format!("http://{listen}/v1/native"),
        None,
        Some("admin"),
    )
    .unwrap();
    let inventory = aterm_json::json!({
        "schema":2,"account":"alice","repositories":[repository("alice", "parent", 1)]
    });
    let admin = client::admin_open(&admin_paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        "gh_access_token",
        TOKEN.as_bytes(),
    )
    .unwrap();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        "github-repositories.json",
        inventory.to_string().as_bytes(),
    )
    .unwrap();
    client::admin_publish(&admin_paths, &admin, &doc).unwrap();
    client::seed_vault_genesis(&admin_paths, &root.path().join("genesis.v2")).unwrap();
    let recipient = client::Paths::for_home(&root.path().join("recipient"));
    client::machine_identity(&recipient).unwrap();
    let device = aterm_secrets::commons::bootstrap::Device::open(&recipient)
        .unwrap()
        .unwrap();
    let bootstrap = aterm_secrets::commons::bootstrap::Bootstrap::required(&admin_paths).unwrap();
    let trust = bootstrap.trust();
    let request = device.request(trust.network, trust.authority).unwrap();
    let allowed = bootstrap
        .device()
        .with_seed(|seed| {
            aterm_secrets::members::Allowed::sign(request.binding(), "recipient", seed)
        })
        .unwrap();
    let handout = client::hand_over(
        &admin_paths,
        &aterm_secrets::members::desk::Handing {
            request: &request,
            node: request.binding().recipient().0,
            name: allowed.name(),
            allowed: &allowed,
            reader: aterm_secrets::members::bundle::Reader::LegacyLog,
            restore: false,
            node_proven: false,
            held: None,
        },
    )
    .unwrap();
    assert!(handout.logged);
    client::write_from_log_bundle(
        &recipient,
        &aterm_secrets::members::Bundle::decode(handout.bundle.as_slice()).unwrap(),
    )
    .unwrap();
    server.stop();
    let logged = materialized_fixture(
        root,
        recipient,
        store.join("native.vault"),
        String::new(),
        true,
        &inventory,
    );
    aterm_secrets::need::test_alab_policy(&logged.paths);
    let parent = Path::new(&logged.who.home).join("parent");
    logged
        .sys
        .stat_of(parent.to_str().unwrap(), stat(true, 0o700));
    repos::tests::script_existing(&logged.sys, &logged.who, &parent, "parent");
    let snapshot = || {
        let config = client::config(&logged.paths).unwrap().unwrap();
        client::with_materialization_ready_for_entries(
            &logged.paths,
            &config,
            &super::super::VAULT_INPUT_ENTRIES,
            || Ok(()),
        )
        .unwrap()
        .unwrap()
        .snapshot
    };
    assert_eq!(snapshot().sequence(), 0);
    assert!(!logged.paths.dir.join("vault").exists());
    aterm_secrets::need::raise_refused(&logged.paths, want.clone(), purpose, 0).unwrap();
    let record = logged.paths.dir.join(aterm_secrets::need::LEDGER);
    let legacy = std::fs::read(&record).unwrap();
    let run_logged = || {
        run_with(
            &logged.sys,
            &logged.ctx,
            Mode::Apply,
            RepositoryWork::Repair,
            None,
        )
        .unwrap()
    };
    let log_api_calls = || {
        logged
            .sys
            .calls()
            .iter()
            .filter(|call| call.contains("agh GET /user"))
            .count()
    };
    logged
        .sys
        .fail_github_api(&logged.who, "/user", crate::port::GithubFailure::Unreached);
    for status in [200, 401, 200] {
        logged.sys.on_github_api(
            &logged.who,
            "/user",
            Ok(GithubResponse {
                status,
                headers: Vec::new(),
                body: br#"{"login":"alice"}"#.to_vec(),
            }),
        );
    }
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Failed);
    assert_eq!(std::fs::read(&record).unwrap(), legacy);
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Changed);
    let accepted = aterm_secrets::need::peek(&logged.paths).unwrap().remove(0);
    assert_eq!(accepted.state, aterm_secrets::need::State::Retired);
    assert_eq!(
        accepted.retired.unwrap().reason,
        aterm_secrets::need::Reason::NotNeeded
    );
    assert_eq!(log_api_calls(), 2);
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Ok);
    assert_eq!(log_api_calls(), 2, "healthy log-only work stays local");

    let legacy = aterm_secrets::need::raise_refused(&logged.paths, want, purpose, 0).unwrap();
    logged.sys.remove(&parent).unwrap();
    let endpoint = "https://github.com/alice/parent.git";
    logged.sys.on(
        &crate::checkout::git(&logged.sys, &logged.who, None)
            .args(["ls-remote", "--get-url", "--", endpoint])
            .display(),
        Ran::ok(endpoint),
    );
    logged.sys.simulate(|_, command| {
        command
            .display()
            .contains("ls-remote --symref --")
            .then(|| Ran::exit(128, "synthetic log-only remote failure"))
    });
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Failed);
    let typed = aterm_secrets::need::peek(&logged.paths).unwrap().remove(0);
    assert_ne!(typed.id, legacy.id);
    assert!(matches!(
        typed.refused,
        Some(aterm_secrets::need::RefusalClaim::Authenticated(_))
    ));
    assert_eq!(log_api_calls(), 3);
    let typed_bytes = std::fs::read(&record).unwrap();
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Failed);
    assert_eq!(log_api_calls(), 3, "same log source does not repeat auth");
    assert_eq!(std::fs::read(&record).unwrap(), typed_bytes);
    logged
        .sys
        .stat_of(parent.to_str().unwrap(), stat(true, 0o700));
    repos::tests::script_existing(&logged.sys, &logged.who, &parent, "parent");
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Failed);
    assert_eq!(
        log_api_calls(),
        3,
        "same-source local ack starts no auth read"
    );
    assert_eq!(std::fs::read(&record).unwrap(), typed_bytes);

    const UPDATED: &str = "fixture_updated_log_token_only";
    assert_eq!(
        client::member_set(
            &admin_paths,
            &client::MemberEntry {
                name: "gh_access_token".into(),
                value: aterm_secrets::SecretBytes::from(UPDATED.as_bytes()),
                if_absent: false,
                anchor: client::Anchor::Keep,
                fleet_reads_anchors: false,
            },
            client::OnBusy::Refuse(std::time::Instant::now() + std::time::Duration::from_secs(60))
        )
        .unwrap(),
        client::MemberWritten::SignedHere
    );
    client::ingest_mesh_records(
        &logged.paths,
        client::load_records(&admin_paths)
            .unwrap()
            .log()
            .unwrap()
            .records()
            .cloned()
            .collect(),
    )
    .unwrap();
    client::sync(&logged.paths, None).unwrap();
    assert_eq!(snapshot().sequence(), 0);
    assert!(!logged.paths.dir.join("vault").exists());
    assert_eq!(
        std::fs::read(logged.paths.home.join(".secrets/gh_access_token")).unwrap(),
        UPDATED.as_bytes()
    );
    logged
        .sys
        .file(logged.token.to_str().unwrap(), UPDATED.as_bytes());
    let helper_read = logged
        .sys
        .calls()
        .into_iter()
        .find(|call| call.contains(" credential fill"))
        .expect("the ordinary route already read back its helper");
    let helper_token = logged.token.clone();
    logged.sys.simulate(move |_, command| {
        (command.display() == helper_read).then(|| {
            let token = aterm_secrets::fsx::read_private(&helper_token, INPUT_MAX)
                .expect("the synchronized fixture token is readable")
                .expect("the synchronized fixture token exists");
            let token = token.as_str().expect("the fixture token is UTF-8");
            Ran::ok(&format!(
                "protocol=https\nhost={GITHUB}\nusername=alice\npassword={token}\n"
            ))
        })
    });
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Changed);
    let accepted = aterm_secrets::need::peek(&logged.paths).unwrap().remove(0);
    assert_eq!(accepted.state, aterm_secrets::need::State::Retired);
    assert_eq!(
        accepted.retired.unwrap().reason,
        aterm_secrets::need::Reason::NotNeeded
    );
    assert_eq!(log_api_calls(), 4);
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Ok);
    assert_eq!(
        log_api_calls(),
        4,
        "accepted changed log source stays local"
    );

    // A later successful clone also goes through the acknowledgment caller. Its
    // ended-request guard must keep this healthy work from another provider read.
    let accepted_bytes = std::fs::read(&record).unwrap();
    logged.sys.remove(&parent).unwrap();
    script::alike(&logged.sys, &logged.who);
    let (_, _, clone) =
        repos::tests::script_clone_named(&logged.sys, &logged.who, "parent", 2, false);
    assert_eq!(stage(&run_logged()).outcome, crate::model::Outcome::Changed);
    assert!(logged.sys.calls().contains(&clone));
    assert_eq!(
        log_api_calls(),
        4,
        "healthy later clone starts no auth read"
    );
    assert_eq!(std::fs::read(&record).unwrap(), accepted_bytes);

    // A stamped envelope may arrive before its records. Accepting its Covers graph
    // refines that same baseline, without claiming causal advancement or sign-in.
    for status in [200, 401] {
        let audience = aterm_secrets::text::Audience::Everyone;
        let root = aterm_tempfile::tempdir().unwrap();
        let start_server = |store: &Path| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let listen = listener.local_addr().unwrap();
            let server = HeldServer(Some(
                aterm_secrets::server::start(
                    listener,
                    &aterm_secrets::server::Options {
                        listen,
                        store: store.to_path_buf(),
                        advertise: None,
                        pins: Vec::new(),
                        hold: std::time::Duration::from_millis(10),
                        keep_awake: false,
                    },
                )
                .unwrap(),
            ));
            (server, format!("http://{listen}/v1/refinement"))
        };
        let (source_server, url) = start_server(&root.path().join("source"));
        let admin_paths = client::Paths::for_home(&root.path().join("admin"));
        let created = client::create(&admin_paths, &url, None, Some("admin")).unwrap();
        let recipient = client::Paths::for_home(&root.path().join("recipient"));
        let recipient_key = client::machine_identity(&recipient)
            .unwrap()
            .recipient()
            .to_bech32();
        let admin = client::admin_open(&admin_paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("recipient", &recipient_key).unwrap();
        doc.set_entry(&audience, "gh_access_token", b"before-refinement")
            .unwrap();
        doc.set_entry(
            &audience,
            "github-repositories.json",
            inventory.to_string().as_bytes(),
        )
        .unwrap();
        client::admin_publish_approved(
            &admin_paths,
            &admin,
            &doc,
            std::slice::from_ref(&recipient_key),
        )
        .unwrap();
        client::seed_vault_genesis(&admin_paths, &root.path().join("genesis.v2")).unwrap();
        let genesis = client::load_records(&admin_paths).unwrap().encode();
        let admin = client::admin_open(&admin_paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&audience, "gh_access_token", TOKEN.as_bytes())
            .unwrap();
        let published = client::admin_publish(&admin_paths, &admin, &doc).unwrap();
        assert!(published.local.is_none(), "the source applied its rotation");
        let aterm_secrets::transport::Fetched::Body(envelope) = aterm_secrets::transport::fetch(
            &aterm_secrets::transport::Source::Url(url.clone()),
            None,
            std::time::Duration::ZERO,
        )
        .unwrap() else {
            panic!("the source serves its actual published envelope");
        };
        let complete = client::load_records(&admin_paths).unwrap().encode();
        let (mirror_server, mirror) = start_server(&root.path().join("mirror"));
        aterm_secrets::transport::publish(
            &aterm_secrets::transport::Source::Url(mirror.clone()),
            &envelope,
        )
        .unwrap();
        client::join(
            &recipient,
            &client::JoinOptions {
                source: format!("{mirror}#{}", created.signer),
                host: Some("recipient".into()),
                interactive: false,
                wait: std::time::Duration::ZERO,
                confirm: &|_| false,
                password: client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(client::load_records(&recipient).unwrap().log().is_none());
        assert_eq!(
            std::fs::read(recipient.home.join(".secrets/gh_access_token")).unwrap(),
            TOKEN.as_bytes()
        );
        // Server guards precede the root owner in this aggregate's drop order.
        struct ServedReadback {
            _servers: [HeldServer; 2],
            fixture: ReadbackFixture,
        }
        let source = root.path().join("mirror/refinement.vault");
        let served = ServedReadback {
            _servers: [source_server, mirror_server],
            fixture: materialized_fixture(root, recipient, source, String::new(), true, &inventory),
        };
        let refined = &served.fixture;
        aterm_secrets::need::test_alab_policy(&refined.paths);
        let current_snapshot = || {
            let config = client::config(&refined.paths).unwrap().unwrap();
            client::with_materialization_ready_for_entries(
                &refined.paths,
                &config,
                &super::super::VAULT_INPUT_ENTRIES,
                || Ok(()),
            )
            .unwrap()
            .unwrap()
            .snapshot
        };
        let current_version = || {
            current_snapshot()
                .authenticated_version(
                    &refined.paths,
                    &aterm_secrets::need::Want::Gh(aterm_secrets::need::GhProfile::Dev).entries(),
                )
                .unwrap()
                .unwrap()
        };
        assert_eq!(current_snapshot().sequence(), published.seq);
        assert!(current_version().claim().envelope().is_some());
        assert!(current_version().claim().log().is_none());
        let parent = Path::new(&refined.who.home).join("parent");
        let endpoint = "https://github.com/alice/parent.git";
        refined.sys.on(
            &crate::checkout::git(&refined.sys, &refined.who, None)
                .args(["ls-remote", "--get-url", "--", endpoint])
                .display(),
            Ran::ok(endpoint),
        );
        refined.sys.simulate(|_, command| {
            command
                .display()
                .contains("ls-remote --symref --")
                .then(|| Ran::exit(128, "synthetic refinement remote failure"))
        });
        refined.sys.on_github_api(
            &refined.who,
            "/user",
            Ok(GithubResponse {
                status: 401,
                headers: Vec::new(),
                body: Vec::new(),
            }),
        );
        refined
            .sys
            .fail_github_api(&refined.who, "/user", crate::port::GithubFailure::Unreached);
        refined.sys.on_github_api(
            &refined.who,
            "/user",
            Ok(GithubResponse {
                status,
                headers: Vec::new(),
                body: br#"{"login":"alice"}"#.to_vec(),
            }),
        );
        let run_refined = || {
            run_with(
                &refined.sys,
                &refined.ctx,
                Mode::Apply,
                RepositoryWork::Repair,
                None,
            )
            .unwrap()
        };
        let api_calls = || {
            refined
                .sys
                .calls()
                .iter()
                .filter(|call| call.contains("agh GET /user"))
                .count()
        };
        assert_eq!(stage(&run_refined()).outcome, crate::model::Outcome::Failed);
        assert_eq!(api_calls(), 1);
        let previous = aterm_secrets::need::peek(&refined.paths).unwrap().remove(0);
        let Some(aterm_secrets::need::RefusalClaim::Authenticated(floor)) = &previous.refused
        else {
            panic!("the actual envelope-backed 401 records its authenticated floor");
        };
        assert!(floor.envelope().is_some());
        assert!(floor.log().is_none());
        let ledger = refined.paths.dir.join(aterm_secrets::need::LEDGER);
        let previous_bytes = std::fs::read(&ledger).unwrap();
        assert_eq!(stage(&run_refined()).outcome, crate::model::Outcome::Failed);
        assert_eq!(api_calls(), 1, "the same envelope starts no auth read");
        aterm_secrets::transport::post_records(&mirror, &genesis).unwrap();
        client::sync(&refined.paths, None).unwrap();
        assert!(
            client::load_records(&refined.paths)
                .unwrap()
                .log()
                .is_some()
        );
        assert!(current_version().claim().log().is_none(), "still Behind");
        assert_eq!(stage(&run_refined()).outcome, crate::model::Outcome::Failed);
        assert_eq!(api_calls(), 1, "a Behind graph starts no auth read");
        assert_eq!(std::fs::read(&ledger).unwrap(), previous_bytes);
        aterm_secrets::transport::post_records(&mirror, &complete).unwrap();
        client::sync(&refined.paths, None).unwrap();
        let version = current_version();
        assert_eq!(version.claim().envelope(), floor.envelope());
        assert!(version.claim().log().is_some());
        assert_eq!(
            version.relation(floor),
            client::version::VersionRelation::Same,
            "accepting Covers is not causal advancement"
        );
        if status == 200 {
            refined
                .sys
                .stat_of(parent.to_str().unwrap(), stat(true, 0o700));
            repos::tests::script_existing(&refined.sys, &refined.who, &parent, "parent");
        }
        assert_eq!(stage(&run_refined()).outcome, crate::model::Outcome::Failed);
        assert_eq!(api_calls(), 2);
        assert_eq!(std::fs::read(&ledger).unwrap(), previous_bytes);
        let rows = run_refined();
        assert_eq!(api_calls(), 3);
        let after = aterm_secrets::need::peek(&refined.paths).unwrap().remove(0);
        if status == 200 {
            assert_eq!(stage(&rows).outcome, crate::model::Outcome::Changed);
            assert_eq!(after.id, previous.id);
            assert_eq!(after.state, aterm_secrets::need::State::Retired);
            assert_eq!(
                after.retired.unwrap().reason,
                aterm_secrets::need::Reason::NotNeeded
            );
            assert_eq!(stage(&run_refined()).outcome, crate::model::Outcome::Ok);
            assert_eq!(api_calls(), 3, "healthy accepted work stays local");
        } else {
            assert_eq!(stage(&rows).outcome, crate::model::Outcome::Failed);
            assert_ne!(after.id, previous.id);
            assert_eq!(
                after.refused,
                Some(aterm_secrets::need::RefusalClaim::Authenticated(
                    version.claim().clone()
                ))
            );
            let strengthened = std::fs::read(&ledger).unwrap();
            assert_eq!(stage(&run_refined()).outcome, crate::model::Outcome::Failed);
            assert_eq!(api_calls(), 3, "the strengthened floor starts no auth read");
            refined
                .sys
                .stat_of(parent.to_str().unwrap(), stat(true, 0o700));
            repos::tests::script_existing(&refined.sys, &refined.who, &parent, "parent");
            assert_eq!(stage(&run_refined()).outcome, crate::model::Outcome::Failed);
            assert_eq!(api_calls(), 3, "local ack cannot erase the refined refusal");
            assert_eq!(std::fs::read(&ledger).unwrap(), strengthened);
        }
    }
}

#[test]
fn captured_repository_readback_at_the_console_reads_no_workstation_census() {
    use aterm_secrets::client;
    let fixture = readback_fixture(true);
    fixture.sys.stat_of("/dev/console", stat(false, 0o600));
    assert!(!crate::console::remote(&fixture.sys));
    fixture.sys.on(
        &crate::facts::hidutil_keyboards().display(),
        Ran::ok("Services:\n"),
    );
    let config = client::config(&fixture.paths).unwrap().unwrap();
    let captured = client::with_materialization_ready_for_entries(
        &fixture.paths,
        &config,
        &super::super::VAULT_INPUT_ENTRIES,
        || {
            InitialRepositoryInputs::capture(&fixture.sys, &fixture.ctx)
                .map_err(aterm_secrets::Error::new)
        },
    )
    .unwrap()
    .unwrap();
    let state = aterm_tempfile::tempdir().unwrap();
    let reading = crate::init::owed_clone_captured(
        &fixture.sys,
        state.path(),
        &crate::init::Holds::NONE,
        captured.inputs,
        || {
            captured
                .snapshot
                .is_current(&fixture.paths)
                .map_err(|why| why.to_string())
        },
        None,
        aterm_secrets::converge::RepositoryWork::Repair,
    )
    .unwrap();
    assert!(matches!(reading, crate::init::OwedClone::Verified { .. }));
    assert!(
        fixture
            .sys
            .calls()
            .iter()
            .all(|call| { !call.contains("hidutil") && call != "keyboard-service-ids" }),
        "repository readback collected workstation facts: {:?}",
        fixture.sys.calls()
    );
}

#[test]
fn captured_automatic_readiness_fences_unlocked_helper_work_with_one_scoped_vault() {
    use crate::init::{Holds, OwedClone};
    use aterm_secrets::client;
    for changed in [
        "none",
        "unrelated",
        "token",
        "writer",
        "profile",
        "capture-profile",
    ] {
        let ReadbackFixture {
            paths,
            source,
            source_text,
            sys,
            who,
            ctx,
            token,
            _root,
        } = readback_fixture(true);
        if changed == "capture-profile" {
            let selected = who.clone();
            let token_path = token.clone();
            sys.on_owned_read(move |sys, path| {
                if path == token_path {
                    scope(sys, &selected);
                }
            });
        }
        let config = client::config(&paths).unwrap().unwrap();
        let captured = client::with_materialization_ready_for_entries(
            &paths,
            &config,
            &super::super::VAULT_INPUT_ENTRIES,
            || InitialRepositoryInputs::capture(&sys, &ctx).map_err(aterm_secrets::Error::new),
        )
        .unwrap()
        .unwrap();
        let final_paths = paths.clone();
        let selected = who.clone();
        let held = Arc::new(Mutex::new(None));
        let retain = Arc::clone(&held);
        sys.simulate(move |sys, command| {
            if command.display().contains("credential fill") {
                // Acquiring the actual writer lock proves the helper runs outside it.
                let writer = client::test_hold_state_lock(&paths).unwrap();
                if changed == "writer" {
                    // A vault pass that changes nothing (the subscriber's over an unchanged
                    // long-poll answer) holds the lock past the helper, until an observation
                    // after it meets the pass and waits on it.
                    *retain.lock().unwrap() = Some(writer);
                } else {
                    drop(writer);
                    if changed == "profile" {
                        scope(sys, &selected);
                    }
                    if matches!(changed, "token" | "unrelated") {
                        aterm_uds::atomic_file::write(
                            &source,
                            if changed == "token" {
                                source_text.replace(TOKEN, "fixture-rotated-token")
                            } else {
                                source_text.replace("unrelated=before", "unrelated=after")
                            }
                            .as_bytes(),
                            0o600,
                        )
                        .unwrap();
                        client::sync(&paths, None).unwrap();
                    }
                }
            }
            None
        });
        let state = aterm_tempfile::tempdir().unwrap();
        let (owed, waited) = client::test_while_state_lock_held(
            move || drop(held.lock().unwrap().take()),
            || {
                crate::init::owed_clone_captured(
                    &sys,
                    state.path(),
                    &Holds::NONE,
                    captured.inputs,
                    || {
                        captured
                            .snapshot
                            .is_current(&final_paths)
                            .map_err(|why| why.to_string())
                    },
                    None,
                    aterm_secrets::converge::RepositoryWork::Repair,
                )
            },
        );
        let owed = owed.unwrap();
        // The pass that changed nothing was waited out and the vault read as it is: no change,
        // where an observation that took the busy lock as its answer would read "writer" as a
        // vault still syncing (`Waiting`).
        let unchanged = matches!(changed, "none" | "unrelated" | "writer");
        assert_eq!(
            matches!(owed, OwedClone::Verified { .. }),
            unchanged,
            "{changed}: {owed:?}"
        );
        assert!(unchanged || matches!(owed, OwedClone::Waiting(_)));
        assert_eq!(waited > 0, changed == "writer", "{changed}: {waited} waits");
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains("credential fill"))
                .count(),
            usize::from(changed != "capture-profile")
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains(" clone ") || call.contains("agh GET"))
        );
    }
}

#[test]
fn native_dependency_batch_checks_stop_and_real_input_changes_after_each_download() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for change in ["none", "stop", "token", "inventory"] {
        let names = ["parent".to_owned()];
        let (sys, item) = listing(&names, &names);
        required_repository(
            &sys,
            &item.user,
            "parent",
            &["vendor/first", "vendor/second"],
        );
        let parent = Path::new(&item.user.home).join("parent");
        super::super::dependencies::tests::download(&sys, &item.user, &parent, "vendor/first", 0);
        super::super::dependencies::tests::download(&sys, &item.user, &parent, "vendor/second", 1);
        let stop = Arc::new(AtomicBool::new(false));
        let pressed = Arc::clone(&stop);
        let who = item.user.clone();
        sys.simulate(move |sys, command| {
            if command.display().contains(" clone ") {
                match change {
                    "stop" => pressed.store(true, Ordering::SeqCst),
                    "token" => {
                        sys.file(
                            &format!("{}/{}", who.home, super::super::TOKEN_FILE),
                            b"fixture_rotated_token",
                        );
                    }
                    "inventory" => metadata(sys, &who, aterm_json::json!([])),
                    _ => {}
                }
            }
            None
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
        let applied = item.apply_guarded(&sys, &ctx, &|| {
            if stop.load(Ordering::SeqCst) {
                Err("late Stop".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(applied.is_ok(), change == "none", "{change}: {applied:?}");
        assert_eq!(
            sys.stat(&parent.join("vendor/first")).is_ok(),
            change == "none",
            "{change}"
        );
        assert_eq!(
            sys.stat(&parent.join("vendor/second")).is_ok(),
            change == "none",
            "{change}"
        );
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains(" clone "))
                .count(),
            if change == "none" { 2 } else { 1 },
            "{change}"
        );
        assert!(sys.stat(&parent).is_ok());
    }
}

#[test]
fn default_credential_reads_private_vault_inventory_without_github_discovery() {
    let (sys, item) = fixture();
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([
            repository("alice", "personal", 1),
            repository("team", "work", 2),
        ]),
    );
    let plan = item.plan(&sys).unwrap();
    assert_eq!(
        plan.repositories
            .iter()
            .map(|repo| (repo.owner.as_str(), repo.name.as_str()))
            .collect::<Vec<_>>(),
        [("alice", "personal"), ("team", "work")]
    );
    assert!(sys.calls().iter().all(|call| !call.contains("/gh ")
        && !call.contains(" agh GET ")
        && !call.contains("auth login")
        && !call.contains(TOKEN)
        && !call.contains("alabsystems")
        && !call.contains("release-credentials")));
    assert!(
        sys.fed().is_empty(),
        "the complete-input pipe retains no token copy"
    );
}

#[test]
fn missing_or_invalid_private_inventory_never_falls_back_to_public_discovery() {
    let (sys, item) = bare();
    sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
    source(&sys, "/Users//alice/.secrets/gh_access_token");
    answer(
        &sys,
        &item.user,
        "/user",
        aterm_json::json!({"login":"alice"}),
    );
    let blocked = item.plan(&sys).err().unwrap();
    assert_eq!(blocked.wait, Some(Wait::RepositoryInventory));
    assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));

    let path = super::super::inventory::file(&item.user.home);
    sys.file(path.to_str().unwrap(), b"not JSON")
        .stat_of(path.to_str().unwrap(), stat(false, 0o600));
    assert_eq!(
        item.plan(&sys).err().unwrap().wait,
        Some(Wait::RepositoryInventory)
    );
    assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
}

#[test]
fn a_captured_private_inventory_stays_with_its_token_snapshot() {
    let (sys, mut item) = fixture();
    metadata(
        &sys,
        &item.user,
        aterm_json::json!([repository("alice", "first", 1)]),
    );
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    metadata(
        &sys,
        &item.user,
        aterm_json::json!([repository("alice", "later", 2)]),
    );
    item.captured = Some(Captured {
        inputs,
        current: Box::new(|| Ok(true)),
    });
    let inputs = &item.captured.as_ref().unwrap().inputs;
    assert_eq!(
        inputs
            .local
            .as_ref()
            .unwrap()
            .inventory
            .as_ref()
            .unwrap()
            .repositories()[0]
            .name,
        "first"
    );
    assert!(!inputs.is_current(&sys).unwrap());
    assert!(
        item.plan(&sys)
            .err()
            .unwrap()
            .why
            .contains("source changed")
    );
    assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
}

#[test]
fn captured_private_inventory_clones_in_the_accounts_background_session() {
    let names = ["background-code".to_owned()];
    let (sys, item) = listing(&names, &[]);
    let mut context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    context.console = None;
    let captured = InitialRepositoryInputs::capture(&sys, &context).unwrap();
    let rows = run_captured(
        &sys,
        &context,
        Mode::Apply,
        RepositoryWork::Refresh,
        captured,
        Box::new(|| Ok(true)),
    )
    .unwrap();
    assert_eq!(
        stage(&rows).outcome,
        crate::model::Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        Reading::of(stage(&rows)),
        Some(Reading::Repositories {
            present: 1,
            count: 1,
            signed_in: true
        })
    );
    assert!(sys.calls().iter().any(|call| call.contains(" clone ")));
    assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
}

#[test]
fn a_native_repository_batch_respects_late_stop_but_not_an_explicit_older_hold() {
    use crate::init::Holds;
    use aterm_types::macos_setup::{Selection, Setting};
    use std::sync::atomic::{AtomicBool, Ordering};
    for late_stop in [false, true] {
        let repositories = names("batch", 6);
        let (sys, item) = listing(&repositories, &[]);
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let stop = Arc::new(AtomicBool::new(false));
        let pressed = Arc::clone(&stop);
        sys.simulate(move |_, command| {
            if late_stop && command.display().contains(" clone ") {
                pressed.store(true, Ordering::SeqCst);
            }
            None
        });
        let mut off = Selection::none();
        off.set(Setting::Repositories, true);
        let start = Holds { off, ..Holds::NONE };
        let current = || {
            let now = Holds {
                stopped: stop.load(Ordering::SeqCst).then(|| "late Stop".to_owned()),
                stopped_revision: Some(2),
                ..start.clone()
            };
            if now.later(&start, Setting::Repositories) {
                Err(now.why(Setting::Repositories))
            } else {
                Ok(())
            }
        };
        assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
        let applied = item.apply_guarded(&sys, &ctx, &current);
        assert_eq!(applied.is_err(), late_stop, "{applied:?}");
        let published = repositories
            .iter()
            .filter(|name| sys.stat(&Path::new(&item.user.home).join(name)).is_ok())
            .count();
        assert_eq!(published, if late_stop { 0 } else { 6 });
        let clones = sys
            .calls()
            .iter()
            .filter(|call| call.contains(" clone "))
            .count();
        if late_stop {
            assert!(
                (1..=LANES_SEEN).contains(&clones),
                "{clones} clones started after Stop"
            );
        } else {
            assert_eq!(clones, 6);
        }
    }
}

#[test]
fn scoped_vault_inputs_publish_through_unrelated_sync_but_refuse_changed_inputs() {
    use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
    for ordinary in [false, true] {
        for changed in ["unrelated", "token", "inventory", "missing", "incomplete"] {
            let root = aterm_tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let source = root.path().join("team.secrets");
            let paths = Paths::for_home(&home);
            let inventory = aterm_json::to_string(&aterm_json::json!({
                "schema":2,"account":"alice","repositories":[repository("alice", "new", 1)]
            }))
            .unwrap();
            let source_text = format!(
                "gh_access_token={TOKEN}\ngithub-repositories.json={inventory}\nunrelated=before\n"
            );
            aterm_uds::atomic_file::write(&source, source_text.as_bytes(), 0o600).unwrap();
            client::join(
                &paths,
                &JoinOptions {
                    source: source.to_string_lossy().into_owned(),
                    host: Some("fixture".into()),
                    interactive: false,
                    wait: std::time::Duration::ZERO,
                    confirm: &|_| false,
                    password: JoinPassword::Given(None),
                    fetched: None,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
            .unwrap();
            let who = User {
                home: home.to_string_lossy().into_owned(),
                ..user("alice", 501)
            };
            let mut sys = Fake::root();
            sys.euid = who.uid;
            sys.logged_in(&who);
            sys.stat_of(home.to_str().unwrap(), stat(true, 0o700));
            sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
            let token_path = home.join(super::super::TOKEN_FILE);
            sys.file(token_path.to_str().unwrap(), TOKEN.as_bytes())
                .stat_of(token_path.to_str().unwrap(), stat(false, 0o600));
            metadata(
                &sys,
                &who,
                aterm_json::json!([repository("alice", "new", 1)]),
            );
            script::launcher(&sys, &who);
            script::lookups_hold(&sys, &who, GITHUB, "alice", TOKEN);
            script::alike(&sys, &who);
            repos::tests::script_clone_named(&sys, &who, "new", 0, false);
            repos::tests::script_existing(&sys, &who, &home.join("new"), "new");
            let mut ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&who));
            ctx.console = None;
            let configured = client::config(&paths).unwrap().unwrap();
            let captured = client::with_materialization_ready_for_entries(
                &paths,
                &configured,
                &super::super::VAULT_INPUT_ENTRIES,
                || InitialRepositoryInputs::capture(&sys, &ctx).map_err(aterm_secrets::Error::new),
            )
            .unwrap()
            .unwrap();
            let syncing = paths.clone();
            let once = std::sync::atomic::AtomicBool::new(false);
            sys.simulate(move |_, command| {
                if command.display().contains(" clone ")
                    && !once.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let next = match changed {
                        "unrelated" => source_text.replace("unrelated=before", "unrelated=after"),
                        "token" => source_text.replace(TOKEN, "fixture-rotated-token"),
                        "inventory" => source_text.replace(&inventory, "{}"),
                        "missing" => source_text
                            .lines()
                            .filter(|line| !line.starts_with("github-repositories.json="))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        "incomplete" => {
                            std::fs::create_dir_all(syncing.home.join(".config/gh/hosts.yml"))
                                .unwrap();
                            format!("{source_text}~/.config/gh/hosts.yml=cannot-materialize\n")
                        }
                        _ => unreachable!(),
                    };
                    aterm_uds::atomic_file::write(&source, next.as_bytes(), 0o600).unwrap();
                    client::sync(&syncing, None).unwrap();
                }
                None
            });
            let rows = if ordinary {
                run(&sys, &ctx, Mode::Apply).unwrap()
            } else {
                run_captured(
                    &sys,
                    &ctx,
                    Mode::Apply,
                    RepositoryWork::Refresh,
                    captured.inputs,
                    Box::new(move || {
                        captured
                            .snapshot
                            .is_current(&paths)
                            .map_err(|why| why.to_string())
                    }),
                )
                .unwrap()
            };
            let published = changed == "unrelated";
            assert_eq!(
                stage(&rows).outcome == crate::model::Outcome::Changed,
                published,
                "{changed}: {rows:?}"
            );
            assert_eq!(sys.stat(&home.join("new")).is_ok(), published, "{changed}");
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains(" clone "))
                    .count(),
                1,
                "{changed}"
            );
        }
    }
}

#[test]
fn ordinary_local_repository_batches_refuse_input_changes_and_allow_unrelated_settings() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for change in ["none", "inventory", "token", "profile", "unrelated"] {
        let names = names("local", 6);
        let (sys, item) = listing(&names, &[]);
        let who = item.user.clone();
        let profile = format!("{}/.config/aterm/init.toml", who.home);
        sys.file(&profile, b"").stat_of(
            &profile,
            Stat {
                len: 0,
                ..stat(false, 0o600)
            },
        );
        let once = AtomicBool::new(false);
        let boundary = Mutex::new(());
        sys.simulate(move |sys, command| {
            if command.display().contains(" clone ") {
                let _mutation = boundary.lock().unwrap();
                if once.swap(true, Ordering::SeqCst) {
                    return None;
                }
                match change {
                    "inventory" => metadata(sys, &who, aterm_json::json!([])),
                    "token" => {
                        sys.file(
                            &format!("{}/{}", who.home, super::super::TOKEN_FILE),
                            b"fixture_rotated_local_token",
                        );
                    }
                    "profile" | "unrelated" => {
                        let path = format!("{}/.config/aterm/init.toml", who.home);
                        let bytes = if change == "profile" {
                            b"[github]\naccount='alice'\nowner='team'\n".as_slice()
                        } else {
                            b"[develop]\ncheckouts=[]\n".as_slice()
                        };
                        sys.file(&path, bytes).stat_of(
                            &path,
                            Stat {
                                len: bytes.len() as u64,
                                ..stat(false, 0o600)
                            },
                        );
                    }
                    _ => {}
                }
            }
            None
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &ctx, Mode::Apply).unwrap();
        let complete = matches!(change, "none" | "unrelated");
        assert_eq!(
            stage(&rows).outcome == crate::model::Outcome::Changed,
            complete,
            "{change}: {rows:?}"
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| sys.stat(&Path::new(&item.user.home).join(name)).is_ok())
                .count(),
            if complete { 6 } else { 0 },
            "{change}"
        );
    }
}

#[test]
fn an_unready_lazy_capture_can_retry_but_a_successful_generation_cannot_rebase() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (sys, item) = fixture();
    let path = Path::new(&item.user.home).join(super::super::TOKEN_FILE);
    let path_for_hook = path.clone();
    let changed = AtomicBool::new(false);
    sys.on_owned_read(move |sys, path| {
        if path == path_for_hook && !changed.swap(true, Ordering::SeqCst) {
            sys.file(path.to_str().unwrap(), b"fixture_changed_while_capturing");
        }
    });
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    assert!(
        matches!(item.probe(&sys, &ctx), Probe::Blocked { .. }),
        "a mixed capture must remain unfinished"
    );
    assert!(
        item.prepared.get().is_none(),
        "an unready capture must not be cached"
    );
    assert!(item.plan(&sys).is_ok(), "a coherent retry may be admitted");
    sys.file(path.to_str().unwrap(), TOKEN.as_bytes());
    assert!(
        item.plan(&sys).is_err(),
        "a successful capture never silently adopts a new generation"
    );
}

/// A private inventory that cannot place every intended repository in home never declares
/// the retained subset complete, even when one matching checkout is already present.
#[test]
fn colliding_private_inventory_leaves_every_checkout_untouched_and_unfinished() {
    use super::repos::tests::script_existing;
    let (sys, item) = fixture();
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([
            repository("team", "Shared", 1),
            repository("alice", "shared", 2),
        ]),
    );
    let path = Path::new(&item.user.home).join("shared");
    sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
    script_existing(&sys, &item.user, &path, "shared");
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    script::alike(&sys, &item.user);
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let rows = run(&sys, &context, Mode::Apply).unwrap();
    assert_eq!(
        stage(&rows).outcome,
        crate::model::Outcome::Blocked,
        "{rows:?}"
    );
    assert_eq!(
        Reading::of(stage(&rows)),
        Some(Reading::Waits(Wait::RepositoryInventory))
    );
    assert!(sys.calls().iter().all(|call| !call.contains(" clone ")
        && !call.contains("--replace-all")
        && !call.contains("credential approve")));
}

#[test]
fn native_inventory_follows_agh_pagination_before_any_mutation() {
    let (sys, item) = scoped_fixture();
    let page = (0..30)
        .map(|index| repository("alice", &format!("repo-{index}"), index + 1))
        .collect::<Vec<_>>();
    let next = format!("https://api.github.com{ACCESSIBLE}&page=2");
    sys.on_github_api(
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        Ok(GithubResponse {
            status: 200,
            headers: vec![(
                "Link".into(),
                format!("<{next}>; rel=\"next\", <{next}>; rel=\"last\""),
            )],
            body: aterm_json::to_vec(&page).unwrap(),
        }),
    );
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=2"),
        aterm_json::json!([repository("alice", "work", 31)]),
    );
    let plan = item.plan(&sys).unwrap();
    assert_eq!(plan.repositories.len(), 31);
    assert_eq!(plan.repositories.last().unwrap().owner, "alice");
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains("credential approve") && !call.contains(" clone "))
    );
}

#[test]
fn failed_native_inventory_never_persists_credentials_or_clones() {
    let (sys, item) = scoped_fixture();
    sys.on_github_api(
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        Err("synthetic incomplete GitHub response".into()),
    );
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    assert!(
        item.apply(&sys, &context)
            .unwrap_err()
            .contains("incomplete")
    );
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains("credential approve")
                && !call.contains("--replace-all")
                && !call.contains(" clone "))
    );
}

#[test]
fn explicit_admitted_profile_uses_its_source_and_scope_and_refuses_identity_mismatch() {
    let (sys, item) = fixture();
    let profile = "/Users//alice/.config/aterm/init.toml";
    let bytes =
        b"[github]\naccount='alice'\nowner='team'\ntoken_file='~/.secrets/custom_development'\n";
    sys.file(profile, bytes).stat_of(
        profile,
        Stat {
            len: bytes.len() as u64,
            ..stat(false, 0o600)
        },
    );
    source(&sys, "/Users//alice/.secrets/custom_development");
    answer(
        &sys,
        &item.user,
        "/users/team",
        aterm_json::json!({"login":"team","type":"Organization"}),
    );
    answer(
        &sys,
        &item.user,
        "/orgs/team/repos?type=all&per_page=30&sort=full_name&direction=asc&page=1",
        aterm_json::json!([repository("team", "work", 1)]),
    );
    assert_eq!(item.plan(&sys).unwrap().repositories[0].owner, "team");
    assert!(!sys.calls().iter().any(|call| call.contains(ACCESSIBLE)));

    let (sys, item) = fixture();
    let bytes = b"[github]\naccount='someone-else'\nowner='someone-else'\ntoken_file='~/.secrets/gh_access_token'\n";
    sys.file(profile, bytes).stat_of(
        profile,
        Stat {
            len: bytes.len() as u64,
            ..stat(false, 0o600)
        },
    );
    let block = item.plan(&sys).err().unwrap();
    assert!(block.why.contains("different configured"));
    assert!(!block.why.contains(TOKEN));
    assert_eq!(block.wait, Some(Wait::OtherAccount));
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.contains("/repos?") || call.contains("credential approve"))
    );
}

/// Every blocked step's reason is decided where it stopped, never from the engine's line, and
/// the Repositories row's note is its words; a step that decides none gives no note. Where
/// only a person ends it (a sign-in, aterm's GitHub settings, a folder in the way, Apple's
/// Command Line Tools), the step is theirs and the row needs them, with their remedy.
#[test]
fn every_blocked_step_says_why_in_system_states_words() {
    use crate::init_observe::observation;
    use aterm_types::macos_setup::{Setting, Standing};
    let profile = "/Users//alice/.config/aterm/init.toml";
    let with_profile = |sys: &Fake, bytes: &[u8]| {
        sys.file(profile, bytes).stat_of(
            profile,
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
    };
    // Git and the token, without GitHub's answer to `/user`.
    let signed_out = || {
        let (sys, item) = bare();
        sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        scope(&sys, &item.user);
        (sys, item)
    };
    let cases: Vec<(&str, (Fake, NativeRepositories), Option<Wait>)> = vec![
        (
            "no git",
            {
                let (sys, item) = bare();
                source(&sys, "/Users//alice/.secrets/gh_access_token");
                metadata(
                    &sys,
                    &item.user,
                    aterm_json::json!([repository("alice", "ready", 1)]),
                );
                (sys, item)
            },
            Some(Wait::GitReadiness),
        ),
        (
            "no token yet",
            {
                let (sys, item) = bare();
                sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
                (sys, item)
            },
            Some(Wait::Token),
        ),
        (
            "a token others can read",
            {
                let (sys, item) = signed_out();
                sys.stat_of("/Users//alice/.secrets/gh_access_token", stat(false, 0o644));
                (sys, item)
            },
            Some(Wait::TokenUnusable),
        ),
        (
            "GitHub unreached",
            {
                let (sys, item) = signed_out();
                sys.fail_github_api(&item.user, "/user", GithubFailure::Unreached);
                (sys, item)
            },
            Some(Wait::Unreached),
        ),
        (
            "a token GitHub refuses",
            {
                let (sys, item) = signed_out();
                sys.fail_github_api(&item.user, "/user", GithubFailure::Unauthenticated);
                (sys, item)
            },
            Some(Wait::TokenRefused),
        ),
        (
            "GitHub's other failure",
            {
                let (sys, item) = signed_out();
                sys.fail_github_api(&item.user, "/user", GithubFailure::Other);
                (sys, item)
            },
            Some(Wait::GitHub),
        ),
        (
            "another account",
            {
                let (sys, item) = fixture();
                with_profile(
                    &sys,
                    b"[github]\naccount='someone-else'\nowner='someone-else'\ntoken_file='~/.secrets/gh_access_token'\n",
                );
                (sys, item)
            },
            Some(Wait::OtherAccount),
        ),
        (
            "settings it cannot read",
            {
                let (sys, item) = fixture();
                with_profile(&sys, b"[github]\nowner='alice'\n");
                (sys, item)
            },
            Some(Wait::Settings),
        ),
        (
            "the inventory unreached",
            {
                let (sys, item) = scoped_fixture();
                sys.fail_github_api(
                    &item.user,
                    &format!("{ACCESSIBLE}&page=1"),
                    GithubFailure::Unreached,
                );
                (sys, item)
            },
            Some(Wait::Unreached),
        ),
        (
            "no room on the disk",
            {
                let (mut sys, item) = fixture();
                sys.volume = Some(volume(9 * GIB));
                answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
                script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
                script::alike(&sys, &item.user);
                (sys, item)
            },
            Some(Wait::DiskSpace),
        ),
        (
            "another login",
            {
                let (mut sys, item) = fixture();
                sys.euid = 502;
                (sys, item)
            },
            None,
        ),
    ];
    for (case, (sys, item), wait) in cases {
        // The pass System State's reading runs: a plan.
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Plan).unwrap();
        // Only a person ends a wait for their settings or folder: their step, which parks a
        // run (design §3.6). A token wait or GitHub not answering is a blocked operation.
        let person = wait.is_some_and(|wait| wait.kind() == FailureKind::NeedsPerson);
        let (outcome, standing) = if person {
            (crate::model::Outcome::Person, Standing::NeedsYou)
        } else {
            (crate::model::Outcome::Blocked, Standing::Waiting)
        };
        assert_eq!(stage(&rows).outcome, outcome, "{case}");
        assert_eq!(
            Reading::of(stage(&rows)),
            wait.map(Reading::Waits),
            "{case}: {rows:?}"
        );
        let seen = observation(Setting::Repositories, &[stage(&rows)]);
        assert_eq!(seen.standing, standing, "{case}");
        assert_eq!(seen.note.as_deref(), wait.map(Wait::note), "{case}");
        let line = stage(&rows).why.as_deref().unwrap();
        assert!(
            seen.note.as_deref().is_none_or(|note| !note.contains(line)),
            "{case}"
        );
    }
}

/// A step that fails as it applies keeps the reason it decided (System State design §3.6):
/// GitHub not answering as the run lists the repositories leaves the failed row the page's
/// words for it (`init_observe::failure_note`), never GitHub's own.
#[test]
fn an_apply_github_did_not_answer_keeps_the_reason_it_decided() {
    let (sys, item) = bare();
    sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
    source(&sys, "/Users//alice/.secrets/gh_access_token");
    scope(&sys, &item.user);
    sys.fail_github_api(&item.user, "/user", GithubFailure::Unreached);
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    assert!(item.apply(&sys, &context).is_err(), "GitHub did not answer");
    assert_eq!(
        item.detail(&sys, &context),
        Some(Reading::Waits(Wait::Unreached).token())
    );
}

/// git's settings with the keychain block an earlier `aterm init` wrote, for the keychain
/// row's probe and apply, then the vault's alone, and the block's replacement answering. The
/// writes, in the order they run.
fn keychain_block_then_vault(sys: &Fake, user: &User) -> Vec<String> {
    script::keychain_block(sys, user, "alice");
    script::keychain_block(sys, user, "alice");
    script::alike(sys, user);
    script::replaces_keychain_block(sys, user)
}

/// The stage's own row: the keychain row comes before it ([`run_with`]).
fn stage(rows: &[crate::model::ItemReport]) -> &crate::model::ItemReport {
    rows.last().expect("the stage's row")
}

/// `[github] enabled = false` holds on this entry point too, which System State reaches with
/// no look at init.toml: the block stays, and nothing writes to git's settings. An init.toml
/// that cannot be admitted says nothing either way, so the row cannot read and writes nothing.
#[test]
fn the_off_switch_keeps_the_keychain_block_on_the_stages_own_run() {
    for (profile, off) in [
        ("[github]\nenabled = false\n", true),
        ("[github]\nowner = \"alice\"\n", false),
    ] {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        keychain_block_then_vault(&sys, &item.user);
        sys.write_user_file(
            &item.user,
            &Path::new(&item.user.home).join(".config/aterm/init.toml"),
            profile.as_bytes(),
            0o600,
        )
        .unwrap();
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply);
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .all(|call| !call.contains("--unset-all") && !call.contains(" --add ")),
            "{profile}: {calls:?}"
        );
        if off {
            let rows = rows.unwrap();
            assert_eq!(
                rows[0].outcome,
                crate::model::Outcome::NotApplicable,
                "{rows:?}"
            );
            assert_eq!(stage(&rows).outcome, crate::model::Outcome::NotApplicable);
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains(" clone ") || call.contains(" agh GET "))
            );
        }
    }
}

/// The keychain block an earlier `aterm init` wrote gives way before anything of the stage
/// runs, and whatever becomes of the stage: with no token on the Mac, and with GitHub out of
/// reach. The login keychain is never asked, nothing is approved into it, and the token
/// enters no command record.
#[test]
fn the_keychain_block_gives_way_first_whatever_the_token_or_github_say() {
    type Setup = fn() -> (Fake, NativeRepositories);
    let cases: [(&str, Setup); 2] = [
        ("no token", || {
            let (sys, item) = bare();
            sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
            script::launcher(&sys, &item.user);
            (sys, item)
        }),
        ("GitHub out of reach", || {
            let (sys, item) = scoped_fixture();
            sys.fail_github_api(
                &item.user,
                &format!("{ACCESSIBLE}&page=1"),
                crate::port::GithubFailure::Unreached,
            );
            (sys, item)
        }),
    ];
    for (label, setup) in cases {
        let (sys, item) = setup();
        let writes = keychain_block_then_vault(&sys, &item.user);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(
            rows[0].outcome,
            crate::model::Outcome::Changed,
            "{label}: {rows:?}"
        );
        assert_ne!(
            stage(&rows).outcome,
            crate::model::Outcome::Changed,
            "{label}"
        );
        let calls = sys.calls();
        let at = |key: &str| calls.iter().position(|call| call == key);
        for write in &writes {
            assert!(at(write).is_some(), "{label}: {calls:?}");
        }
        if let Some(asked) = calls.iter().position(|call| call.contains("agh GET")) {
            assert!(at(&writes[writes.len() - 1]) < Some(asked), "{calls:?}");
        }
        assert!(
            calls.iter().all(|call| !call.contains(TOKEN)
                && !call.contains("/gh ")
                && !call.contains("credential fill")
                && !call.contains("credential approve")
                && !call.contains("credential-osxkeychain")
                && !call.contains("--replace-all")
                && !call.contains("auth login")),
            "{label}: {calls:?}"
        );
        assert!(sys.fed().is_empty(), "{label}");
    }
}

/// aterm's route answers past a keychain helper it did not write (CLT's own, with no vault
/// reset): the stage needs nothing of the person's git settings, which stay as they are.
#[test]
fn a_keychain_helper_aterm_did_not_write_neither_stops_the_stage_nor_is_asked() {
    let context = crate::items::defaults::tests::ctx(&[user("alice", 501)]);
    let (sys, item) = fixture();
    present_repository(&sys, &item.user, "ready");
    script::unconfigured(&sys, &item.user);
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    let rows = run(&sys, &context, Mode::Apply).unwrap();
    assert_eq!(rows[0].outcome, crate::model::Outcome::NotApplicable);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    let calls = sys.calls();
    assert!(calls.contains(&script::helpers(&item.user)), "{calls:?}");
    assert!(calls.contains(&script::fill(&item.user)), "{calls:?}");
    assert!(
        calls.iter().all(|call| !call.contains("--unset-all")
            && !call.contains("--add")
            && !call.contains("credential approve")
            && !call.contains("--global")
            && !call.contains("osxkeychain")),
        "{calls:?}"
    );
}

#[test]
fn an_apply_journals_in_the_journal_of_the_system_it_runs_on_and_a_plan_in_none() {
    let journaled = |sys: &Fake| -> Vec<String> {
        sys.written(Fake::JOURNAL)
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
                line["id"].as_str().unwrap().to_owned()
            })
            .collect()
    };
    for mode in [Mode::Plan, Mode::Apply] {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        keychain_block_then_vault(&sys, &item.user);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, mode).unwrap();
        let want = if mode == Mode::Apply {
            (
                crate::model::Outcome::Changed,
                vec![format!("init.github.keychain@{}", item.user.name)],
            )
        } else {
            (crate::model::Outcome::Planned, Vec::new())
        };
        assert_eq!(
            (rows[0].outcome, journaled(&sys)),
            want,
            "{mode:?}: {rows:?}"
        );
    }
}

/// aterm's route is read, never rewritten: one that hands back the synced token is ready,
/// and one that hands back another, or none, stops the row, which says why, with nothing
/// written and every worktree in place: no stage is done while a clone would fail. Nothing
/// goes into the keychain or rewrites git's settings.
#[test]
fn lookups_are_read_and_never_rewritten() {
    let context = |item: &NativeRepositories| {
        crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user))
    };
    let unwritten = |sys: &Fake| {
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve")
                    && !call.contains("--global")
                    && !call.contains("--unset-all")
                    && !call.contains("--replace-all")),
            "{:?}",
            sys.calls()
        );
    };
    let (sys, item) = fixture();
    present_repository(&sys, &item.user, "ready");
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    match item.probe(&sys, &context(&item)) {
        Probe::Ok { now } => assert!(now.contains("hands back the synced token"), "{now}"),
        other => panic!("{other:?}"),
    }
    item.apply(&sys, &context(&item)).unwrap();
    unwritten(&sys);

    // Lookups that hand back a token the vault rotated out, and ones that hand back none.
    for (held, said) in [
        (Some("a_rotated_out_value"), "another credential"),
        (None, "hands back no token"),
    ] {
        let (sys, item) = fixture();
        present_repository(&sys, &item.user, "ready");
        match held {
            Some(held) => script::lookups_hold(&sys, &item.user, GITHUB, "alice", held),
            None => script::lookups_hold_nothing(&sys, &item.user),
        }
        match item.probe(&sys, &context(&item)) {
            Probe::Blocked { why } => {
                assert!(why.contains(said), "{why}");
                assert!(!why.contains("a_rotated_out_value"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        unwritten(&sys);
    }
}

const GIB: u64 = aterm_capacity::floor::GIB;

/// A data volume of 100 GiB with `free` bytes free and a floor of 8 GiB.
fn volume(free: u64) -> aterm_capacity::reading::Volume {
    aterm_capacity::reading::Volume {
        mount: "/System/Volumes/Data".into(),
        total: 100 * GIB,
        free,
        floor: 8 * GIB,
        room: 8 * GIB,
    }
}

/// One repository GitHub sizes at 2 GiB, whose clone takes about 10 GiB.
fn huge() -> aterm_json::Value {
    aterm_json::json!([{"id":1,"name":"huge","owner":{"login":"alice"},
        "full_name":"alice/huge","private":true,
        "clone_url":"https://github.com/alice/huge.git",
        "default_branch":"main","size":2 * 1024 * 1024}])
}

/// The first-launch inventory leaves a repository the disk has no room for above the disk
/// watch's floor and names it; with no reading of the volume it tries the clone.
#[test]
fn a_repository_the_disk_has_no_room_for_is_not_cloned() {
    // One that fits beside one that does not: the plan changes, so the one that fits arrives.
    let (mut sys, item) = fixture();
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    sys.volume = Some(volume(12 * GIB));
    let mut both = huge();
    both.as_array_mut()
        .unwrap()
        .push(repository("alice", "small", 2));
    answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), both);
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    script::alike(&sys, &item.user);
    let probed = item.probe(&sys, &context);
    assert!(matches!(probed, Probe::Change { .. }), "{probed:?}");

    for read in [true, false] {
        let (mut sys, item) = fixture();
        sys.volume = read.then(|| volume(12 * GIB));
        answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        // Room comes back by itself, so the row waits and the run tries again; it never
        // parks the run as a person's step would (design §3.6).
        let probed = item.probe(&sys, &context);
        assert_eq!(matches!(probed, Probe::Blocked { .. }), read, "{probed:?}");
        let why = item.apply(&sys, &context).unwrap_err();
        let tried = sys.calls().iter().any(|call| call.contains("alice/huge"));
        if read {
            assert_eq!(
                why,
                "1 repository not cloned, for want of disk space: ~/huge (about 10 GiB); \
                 /System/Volumes/Data has 12 GiB free and keeps 8 GiB free; free space, \
                 then press Check again in System State, or launch aterm again"
            );
            assert!(!tried, "{:?}", sys.calls());
        } else {
            assert!(tried, "{why}: {:?}", sys.calls());
        }
    }

    // The disk fills between the plan and the pass: the failed row keeps the reason the
    // pass decided, for the page (`init_observe::failure_note`).
    let (mut sys, item) = fixture();
    sys.volume = Some(volume(60 * GIB));
    answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    script::alike(&sys, &item.user);
    let probed = item.probe(&sys, &context);
    assert!(matches!(probed, Probe::Change { .. }), "{probed:?}");
    sys.volume = Some(volume(12 * GIB));
    assert!(item.apply(&sys, &context).is_err());
    assert_eq!(
        item.detail(&sys, &context),
        Some(Reading::Waits(Wait::DiskSpace).token())
    );

    // A vault snapshot that went stale outranks the room: the run waits for the new one,
    // whose repositories and token may differ, instead of for disk space.
    let (mut sys, mut item) = fixture();
    sys.volume = Some(volume(12 * GIB));
    answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
    let inputs = InitialRepositoryInputs::capture(&sys, &context).unwrap();
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    script::alike(&sys, &item.user);
    let sys = Arc::new(sys);
    let observed = Arc::clone(&sys);
    // Stale from the sign-in lookup on, which the plan's own checks come before.
    item.captured = Some(Captured {
        inputs,
        current: Box::new(move || {
            Ok(!observed
                .calls()
                .iter()
                .any(|call| call.contains("credential fill")))
        }),
    });
    match item.probe(&*sys, &context) {
        Probe::Blocked { why } => assert!(why.contains("vault changed"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(item.detail(&*sys, &context), None);
}

#[test]
fn lookups_that_hand_back_another_token_stop_before_any_clone_and_withhold_output() {
    let (sys, item) = fixture();
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([repository("alice", "new", 1)]),
    );
    script::alike(&sys, &item.user);
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", "unrelated_private_value");
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let why = item.apply(&sys, &context).unwrap_err();
    assert!(why.contains("does not hand back the synced token"), "{why}");
    assert!(!why.contains("unrelated_private_value") && !why.contains(TOKEN));
    assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
}

#[test]
fn captured_repository_inputs_do_not_reread_a_new_synced_token() {
    let (sys, mut item) = fixture();
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    let token_path = format!("{}/.secrets/gh_access_token", item.user.home);
    let newer = "fixture_partial_sync_token_only";
    sys.file(&token_path, newer.as_bytes()).stat_of(
        &token_path,
        Stat {
            len: newer.len() as u64,
            ..stat(false, 0o600)
        },
    );
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([]),
    );
    item.captured = Some(Captured {
        inputs,
        current: Box::new(|| Ok(true)),
    });
    let inputs = &item.captured.as_ref().unwrap().inputs;
    assert_eq!(
        inputs.local.as_ref().unwrap().token.bytes(),
        TOKEN.as_bytes()
    );
    assert!(!inputs.is_current(&sys).unwrap());
    assert!(
        item.plan(&sys)
            .err()
            .unwrap()
            .why
            .contains("source changed")
    );
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains(TOKEN) && !call.contains(newer))
    );
}

#[test]
fn a_vault_version_changed_during_inventory_refuses_all_local_effects() {
    let (sys, mut item) = fixture();
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([]),
    );
    let checked = std::sync::atomic::AtomicUsize::new(0);
    item.captured = Some(Captured {
        inputs,
        current: Box::new(move || {
            Ok(checked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0)
        }),
    });
    let why = item.apply(&sys, &ctx).unwrap_err();
    assert!(why.contains("changed"), "{why}");
    assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains("credential approve")
                && !call.contains("--global")
                && !call.contains(" clone "))
    );
}

#[test]
fn a_ready_captured_run_without_clones_preserves_compatible_live_helpers() {
    let (sys, mut item) = fixture();
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    present_repository(&sys, &item.user, "kept");
    let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    script::alike(&sys, &item.user);
    item.captured = Some(Captured {
        inputs,
        current: Box::new(|| Ok(true)),
    });
    item.apply(&sys, &ctx).unwrap();
    let calls = sys.calls();
    assert!(calls.contains(&script::fill(&item.user)), "{calls:?}");
    assert!(
        calls
            .iter()
            .all(|call| !call.contains("credential approve") && !call.contains("--global"))
    );
}

/// A captured run's clones go through aterm's route and never write the login keychain.
#[test]
fn captured_clones_never_write_the_login_keychain() {
    let (sys, mut item) = fixture();
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([repository("alice", "new", 1)]),
    );
    let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    item.captured = Some(Captured {
        inputs,
        current: Box::new(|| Ok(true)),
    });
    // The clone itself is not scripted: it stops at its first git run.
    assert!(item.apply(&sys, &ctx).is_err());
    let calls = sys.calls();
    assert!(
        calls.iter().all(|call| !call.contains("credential approve")
            && !call.contains("osxkeychain")
            && !call.contains(TOKEN)
            && !call.contains("--global")
            && !call.contains(" clone ")),
        "{calls:?}"
    );
}

/// A Mac whose GitHub account lists `listed` (each alice's, sized `0`, in GitHub's order),
/// of which `present` are already worktrees in `~` and every other one is scripted to clone
/// into the staging directories a pass makes in list order; aterm's route hands back the
/// synced token.
fn listing(listed: &[String], present: &[String]) -> (Fake, NativeRepositories) {
    listing_with(listed, present, &[])
}

/// [`listing`], with the worktrees named in `theirs` cloned from someone else's origin.
fn listing_with(
    listed: &[String],
    present: &[String],
    theirs: &[&str],
) -> (Fake, NativeRepositories) {
    use super::repos::tests::{script_clone_named, script_existing};
    let (sys, item) = fixture();
    let home = Path::new(&item.user.home);
    let values: Vec<aterm_json::Value> = listed
        .iter()
        .zip(1..)
        .map(|(name, id)| repository("alice", name, id))
        .collect();
    metadata(&sys, &item.user, aterm_json::Value::Array(values));
    let mut sequence = 0;
    for name in listed {
        let path = home.join(name);
        if theirs.contains(&name.as_str()) || present.contains(name) {
            sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
        } else if !repos::HOME_FOLDERS
            .iter()
            .any(|folder| folder.eq_ignore_ascii_case(name))
        {
            script_clone_named(&sys, &item.user, name, sequence, false);
            sequence += 1;
        }
        script_existing(&sys, &item.user, &path, name);
    }
    if !theirs.is_empty() {
        let foreign: Vec<_> = theirs
            .iter()
            .map(|name| {
                (
                    crate::checkout::git(&sys, &item.user, Some(&home.join(name)))
                        .args(["config", "--get-all", "remote.origin.url"])
                        .display(),
                    format!("https://github.com/someone/{name}.git\n"),
                )
            })
            .collect();
        sys.simulate(move |_, command| {
            let key = command.display();
            foreign
                .iter()
                .find(|(expected, _)| expected == &key)
                .map(|(_, origin)| Ran::ok(origin))
        });
    }
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    script::alike(&sys, &item.user);
    (sys, item)
}

fn names(prefix: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{prefix}-{index:03}"))
        .collect()
}

/// Default setup must refresh a clean acquired checkout, even when its local
/// origin refs still describe the old commit. Only the transport port substitutes
/// this private bare origin; the item admits the normal private GitHub identity.
/// Independent stores overlap within four lanes, while source withdrawal,
/// storage replacement and offline failures stop queued work without moving it, and so
/// does a checkout made a partial clone after its first read (`late partial`): the
/// storage is read again before the fetch. Its reads of the remote's tip before that never
/// fetch it lazily, where git honours `GIT_NO_LAZY_FETCH` (2.45 and later, and the May 2024
/// security releases from 2.39.4 on, which distributions carry to older versions): no
/// promisor pack appears, though origin is reachable (its URL rewritten to the local origin).
/// The fake models this host and selects its native git, which the real runs use.
/// A git before 2.45, which refuses `--no-lazy-fetch` (a stand-in over that git, so on
/// every host), refreshes a clean checkout alike: nothing the refresh runs carries the
/// option, and it leaves a partial clone, the one git could fetch lazily in, as it is.
/// NEGATIVE CONTROLS: the stand-in does refuse the option, and a refresh that read storage
/// with it (as before 2026-11-08) leaves the clean checkout behind.
#[test]
fn default_apply_refreshes_a_clean_checkout_beyond_its_cached_origin() {
    use crate::port::Live;
    let git = crate::items::github::need_native_git();
    // An empty clone is complete, but its origin can acquire its first commit later.
    // Keep the real Git state in one private home and route only this fixture's
    // advertised GitHub origin to its own bare repository.
    for preserved in [
        "none",
        "untracked",
        "ignored",
        "staged",
        "custom branch",
        "late ignored",
        "recorded refresh",
        "recorded repair",
    ] {
        let live = Live::new();
        let repos = crate::checkout::tests::Repos::new(&live, "native-unborn-refresh");
        let empty = Path::new(&repos.who.home).join("empty.git");
        repos.init(&empty, true);
        let work = Path::new(&repos.who.home).join("unborn");
        repos.clone_into(&empty, &work);
        let origin = format!("https://github.com/{}/unborn.git", repos.who.name);
        repos.g(&work, &["remote", "set-url", "origin", &origin]);
        let head = || {
            live.run_complete_stdout(&crate::checkout::git(&live, &repos.who, Some(&work)).args([
                "rev-parse",
                "--verify",
                "--quiet",
                "HEAD^{commit}",
            ]))
            .unwrap()
        };
        assert!(!head().success(), "the actual empty clone has no commit");

        let mut sys = Fake::root();
        sys.euid = repos.who.uid;
        sys.linux = !live.macos();
        sys.logged_in(&repos.who)
            .stat_of(
                &repos.who.home,
                live.stat(Path::new(&repos.who.home)).unwrap(),
            )
            .executable(git, b"selected fixture git");
        for path in [work.clone(), work.join(".git"), work.join(".git/objects")] {
            sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
            sys.file_id_of(&path, live.file_id(&path).unwrap());
        }
        script::launcher(&sys, &repos.who);
        owned_source(&sys, &repos.who);
        metadata(
            &sys,
            &repos.who,
            aterm_json::json!([repository(&repos.who.name, "unborn", 1)]),
        );
        script::lookups_hold(&sys, &repos.who, GITHUB, &repos.who.name, TOKEN);
        script::alike(&sys, &repos.who);
        if preserved.starts_with("recorded ") {
            let path = Path::new(&repos.who.home).join(".config/aterm/init.toml");
            let bytes = b"[develop]\ncheckouts=['~/unborn']\n";
            sys.file(path.to_str().unwrap(), bytes).stat_of(
                path.to_str().unwrap(),
                Stat {
                    uid: repos.who.uid,
                    gid: repos.who.gid,
                    len: bytes.len() as u64,
                    ..stat(false, 0o600)
                },
            );
        }
        let (at, bare, advertised) = (
            work.clone(),
            empty.clone(),
            std::ffi::OsString::from(origin.clone()),
        );
        let late_ignored = Arc::new(Mutex::new(None));
        let injected = Arc::clone(&late_ignored);
        sys.simulate(move |sys, command| {
            let in_work = command
                .args
                .windows(2)
                .any(|pair| pair[0] == "-C" && pair[1] == at.as_os_str());
            if !in_work
                && !command
                    .args
                    .iter()
                    .any(|arg| arg.as_os_str() == advertised.as_os_str())
            {
                return None;
            }
            let remote = command.args.iter().any(|arg| arg == "ls-remote")
                && !command.args.iter().any(|arg| arg == "--get-url");
            let fetch = command.args.iter().any(|arg| arg == "fetch");
            let mut command = command.clone();
            if remote || fetch {
                for arg in &mut command.args {
                    if arg == "origin" || arg.as_os_str() == advertised.as_os_str() {
                        *arg = bare.as_os_str().to_owned();
                    }
                }
            }
            if fetch {
                command
                    .args
                    .push("+refs/heads/*:refs/remotes/origin/*".into());
            }
            let late_collision =
                preserved == "late ignored" && command.args.iter().any(|arg| arg == "checkout");
            if late_collision {
                // The last producer census has passed; Git must protect this
                // ignored collision at the actual checkout boundary.
                let mut before = injected.lock().unwrap();
                assert!(before.is_none(), "the first checkout is attempted once");
                let path = at.join("README");
                std::fs::write(&path, b"late ignored owner work\n").unwrap();
                let live = Live::new();
                sys.file(path.to_str().unwrap(), b"late ignored owner work\n")
                    .stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
                *before = Some(live.file_id(&path).unwrap());
            }
            let result = Live::new().run_complete_stdout(&command).unwrap();
            if late_collision {
                assert!(!result.success(), "Git refuses the late ignored collision");
            }
            // The fake mirrors the index the real git leaves in the worktree, which a
            // checkout with a commit must have to read as finished
            // (`repos::WORKTREE_READ`'s `--git-path index`).
            let index = at.join(".git/index");
            if let Ok(written) = Live::new().stat(&index) {
                sys.stat_of(index.to_str().unwrap(), written);
            }
            Some(result)
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&repos.who));
        let empty_pass = run_captured(
            &sys,
            &ctx,
            Mode::Apply,
            RepositoryWork::Refresh,
            InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
            Box::new(|| Ok(true)),
        )
        .unwrap();
        assert!(
            matches!(
                stage(&empty_pass).outcome,
                crate::model::Outcome::Ok | crate::model::Outcome::Changed
            ),
            "an actually empty origin is complete: {empty_pass:?}"
        );
        assert_eq!(
            Reading::of(stage(&empty_pass)),
            Some(Reading::Repositories {
                present: 1,
                count: 1,
                signed_in: true
            })
        );
        assert!(!head().success(), "empty readiness invents no local commit");

        let owned = work.join("owner-notes");
        if matches!(preserved, "untracked" | "ignored" | "staged") {
            std::fs::write(&owned, b"the person's unfinished work\n").unwrap();
            sys.file(owned.to_str().unwrap(), b"the person's unfinished work\n")
                .stat_of(owned.to_str().unwrap(), live.stat(&owned).unwrap());
        }
        if matches!(preserved, "ignored" | "late ignored") {
            std::fs::create_dir_all(work.join(".git/info")).unwrap();
        }
        if preserved == "ignored" {
            std::fs::write(work.join(".git/info/exclude"), b"owner-notes\n").unwrap();
            assert!(
                repos.status(&work).is_empty(),
                "normal status hides ignored files"
            );
        } else if preserved == "staged" {
            repos.g(&work, &["add", "--", "owner-notes"]);
        } else if preserved == "custom branch" {
            repos.g(&work, &["symbolic-ref", "HEAD", "refs/heads/topic"]);
        } else if preserved == "late ignored" {
            std::fs::write(work.join(".git/info/exclude"), b"README\n").unwrap();
        }
        let owned_before = matches!(preserved, "untracked" | "ignored" | "staged")
            .then(|| live.file_id(&owned).unwrap());
        let branch_before = std::fs::read(work.join(".git/HEAD")).unwrap();
        let index = || match std::fs::read(work.join(".git/index")) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("private fixture index: {error}"),
        };
        let index_before = index();
        if matches!(
            preserved,
            "untracked" | "ignored" | "staged" | "custom branch"
        ) {
            let still_empty = run_captured(
                &sys,
                &ctx,
                Mode::Apply,
                RepositoryWork::Refresh,
                InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
                Box::new(|| Ok(true)),
            )
            .unwrap();
            assert!(matches!(
                stage(&still_empty).outcome,
                crate::model::Outcome::Ok | crate::model::Outcome::Changed
            ));
            assert_eq!(
                Reading::of(stage(&still_empty)),
                Some(Reading::Repositories {
                    present: 1,
                    count: 1,
                    signed_in: true
                }),
                "{preserved}: an actually empty origin remains complete"
            );
            assert!(
                !head().success(),
                "{preserved}: no empty-origin commit invented"
            );
            assert_eq!(
                std::fs::read(work.join(".git/HEAD")).unwrap(),
                branch_before
            );
            assert_eq!(
                index(),
                index_before,
                "{preserved}: empty-origin index preserved"
            );
            if let Some(identity) = owned_before {
                assert_eq!(
                    std::fs::read(&owned).unwrap(),
                    b"the person's unfinished work\n"
                );
                assert_eq!(live.file_id(&owned).unwrap(), identity);
            }
        }
        // The publisher adds its first commit after this clone was legitimately empty.
        repos.g(
            &repos.seed,
            &["push", "--quiet", empty.to_str().unwrap(), "main"],
        );
        let expected = repos.head(&repos.seed);
        let applied = run_captured(
            &sys,
            &ctx,
            Mode::Apply,
            if preserved == "recorded repair" {
                RepositoryWork::Repair
            } else {
                RepositoryWork::Refresh
            },
            InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
            Box::new(|| Ok(true)),
        )
        .unwrap();
        let actual = head();
        if preserved == "none" {
            assert!(
                actual.success(),
                "a formerly empty clone must acquire its origin's first commit: {applied:?}"
            );
            assert_eq!(actual.out().trim(), expected);
            assert_eq!(std::fs::read(work.join("README")).unwrap(), b"one\n");
            assert_eq!(
                Reading::of(stage(&applied)),
                Some(Reading::Repositories {
                    present: 1,
                    count: 1,
                    signed_in: true
                })
            );
        } else {
            assert!(
                !actual.success(),
                "{preserved}: owner work was not initialized over"
            );
            assert_eq!(
                std::fs::read(work.join(".git/HEAD")).unwrap(),
                branch_before
            );
            assert_eq!(index(), index_before, "{preserved}: index preserved");
            if let Some(identity) = owned_before {
                assert_eq!(
                    std::fs::read(&owned).unwrap(),
                    b"the person's unfinished work\n"
                );
                assert_eq!(live.file_id(&owned).unwrap(), identity);
            }
            if preserved == "late ignored" {
                let identity = late_ignored
                    .lock()
                    .unwrap()
                    .expect("the actual checkout reached the late collision");
                assert_eq!(
                    std::fs::read(work.join("README")).unwrap(),
                    b"late ignored owner work\n"
                );
                assert_eq!(live.file_id(&work.join("README")).unwrap(), identity);
            } else {
                assert!(
                    !work.join("README").exists(),
                    "{preserved}: no first checkout"
                );
            }
            assert_eq!(
                Reading::of(stage(&applied)),
                Some(Reading::Repositories {
                    present: 0,
                    count: 1,
                    signed_in: true
                }),
                "{preserved}: an unborn preserved worktree is unfinished"
            );
        }
        assert!(sys.calls().iter().all(|call| !call.contains(" reset ")
            && !call.contains(" clean ")
            && !call.contains(" stash ")));
    }
    // A git before 2.45 as git 2.43 is one: `--no-lazy-fetch` is an unknown option (129).
    let scratch = aterm_tempfile::tempdir().unwrap();
    let old_git = scratch.path().join("old-git");
    aterm_tempfile::stand_in(
        &old_git,
        &format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = --no-lazy-fetch ]; then\n    \
             echo 'unknown option: --no-lazy-fetch' >&2\n    exit 129\n  fi\ndone\n\
             exec '{git}' \"$@\"\n"
        ),
    )
    .unwrap();
    let refused = Live::new()
        .run_complete_stdout(
            &crate::port::Cmd::new(&old_git).args(["--no-lazy-fetch", "--version"]),
        )
        .unwrap();
    assert_eq!(
        refused.status,
        Some(129),
        "control: the stand-in is a git before 2.45"
    );
    for case in [
        "clean",
        "old git",
        "old git partial",
        "dirty",
        "topic",
        "recorded",
        "shallow",
        "partial",
        "retargeted",
        "late retargeted",
        "late partial",
        "changed source",
        "late recorded",
        "unchanged develop",
        "parallel",
        "parallel stopped",
        "parallel rebound",
        "parallel offline",
        "serial unknown",
        "serial shared",
    ] {
        let live = Live::new();
        let repos = crate::checkout::tests::Repos::new(&live, "native-refresh");
        let before = repos.head(&repos.work);
        let mut works = vec![repos.work.clone()];
        let parallel = case.starts_with("parallel");
        let multiple = parallel || case.starts_with("serial");
        if multiple {
            for index in 1..6 {
                let path = Path::new(&repos.who.home).join(format!("work{index}"));
                repos.clone_into(&repos.origin, &path);
                works.push(path);
            }
        }
        for path in &works {
            let name = path.file_name().unwrap().to_str().unwrap();
            let origin = format!("https://github.com/{}/{name}.git", repos.who.name);
            repos.g(path, &["remote", "set-url", "origin", &origin]);
        }
        let tip = repos.advance("new-source.txt", "newly published source\n");
        match case {
            "dirty" => std::fs::write(repos.work.join("README"), b"preserved edits\n").unwrap(),
            "topic" => {
                repos.g(&repos.work, &["switch", "--quiet", "-c", "topic"]);
            }
            "shallow" => {
                std::fs::write(repos.work.join(".git/shallow"), format!("{before}\n")).unwrap()
            }
            "partial" | "old git partial" => {
                repos.g(
                    &repos.work,
                    &["config", "extensions.partialClone", "origin"],
                );
            }
            "retargeted" => {
                repos.g(&repos.work, &["config", "branch.main.remote", "other"]);
            }
            _ => {}
        }
        assert_eq!(repos.head(&repos.work), before);
        assert_eq!(
            repos.g(&repos.work, &["rev-parse", "refs/remotes/origin/main"]),
            before
        );

        let mut sys = Fake::root();
        sys.euid = repos.who.uid;
        sys.linux = !live.macos();
        sys.logged_in(&repos.who)
            .stat_of(
                &repos.who.home,
                Stat {
                    uid: repos.who.uid,
                    gid: repos.who.gid,
                    ..stat(true, 0o700)
                },
            )
            .executable(git, b"selected fixture git")
            .stat_of(
                repos.work.to_str().unwrap(),
                Stat {
                    uid: repos.who.uid,
                    gid: repos.who.gid,
                    ..stat(true, 0o700)
                },
            );
        for work in &works {
            for path in [work.clone(), work.join(".git"), work.join(".git/objects")] {
                sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
                if case != "serial unknown" {
                    let subject = if case == "serial shared" && path.ends_with(".git/objects") {
                        repos.work.join(".git/objects")
                    } else {
                        path.clone()
                    };
                    sys.file_id_of(&path, live.file_id(&subject).unwrap());
                }
            }
            // The real checkout's index, which its finished clone wrote.
            let index = work.join(".git/index");
            sys.stat_of(index.to_str().unwrap(), live.stat(&index).unwrap());
        }
        script::launcher(&sys, &repos.who);
        owned_source(&sys, &repos.who);
        metadata(
            &sys,
            &repos.who,
            aterm_json::Value::Array(
                works
                    .iter()
                    .enumerate()
                    .map(|(index, path)| {
                        repository(
                            &repos.who.name,
                            path.file_name().unwrap().to_str().unwrap(),
                            index as u64 + 1,
                        )
                    })
                    .collect(),
            ),
        );
        if case == "recorded" {
            let path = Path::new(&repos.who.home).join(".config/aterm/init.toml");
            let bytes = b"[develop]\ncheckouts=['~/work']\n";
            sys.file(path.to_str().unwrap(), bytes).stat_of(
                path.to_str().unwrap(),
                Stat {
                    uid: repos.who.uid,
                    gid: repos.who.gid,
                    len: bytes.len() as u64,
                    ..stat(false, 0o600)
                },
            );
        }
        script::lookups_hold(&sys, &repos.who, GITHUB, &repos.who.name, TOKEN);
        script::alike(&sys, &repos.who);
        // Whether this git honours `GIT_NO_LAZY_FETCH`: a read it bars, in a partial clone of
        // origin that lacks the tip, fetches nothing.
        let honours_no_lazy_fetch = case == "late partial" && {
            let probe = Path::new(&repos.who.home).join("lazy-probe");
            repos.init(&probe, false);
            let origin = repos.origin.to_string_lossy();
            repos.g(&probe, &["remote", "add", "origin", &origin]);
            repos.g(&probe, &["config", "extensions.partialClone", "origin"]);
            !live
                .run_complete_stdout(
                    &crate::checkout::git(&live, &repos.who, Some(&probe))
                        .env("GIT_NO_LAZY_FETCH", "1")
                        .args(["cat-file", "-e", &tip]),
                )
                .unwrap()
                .success()
        };
        let old = case.starts_with("old git").then(|| old_git.clone());
        let paths = works.clone();
        let bare = repos.origin.clone();
        let inventory = super::super::inventory::file(&repos.who.home);
        let profile = Path::new(&repos.who.home).join(".config/aterm/init.toml");
        let (uid, gid) = (repos.who.uid, repos.who.gid);
        let who = repos.who.clone();
        let gate = Arc::new((Mutex::new((0_usize, 0_usize)), std::sync::Condvar::new()));
        let seen = Arc::clone(&gate);
        sys.simulate(move |sys, command| {
            let work = paths
                .iter()
                .find(|path| command.args.iter().any(|arg| arg == path.as_os_str()))?;
            let mut command = command.clone();
            if let Some(old) = &old {
                command.program = old.clone();
            }
            let remote = command.args.iter().any(|arg| arg == "--symref");
            if remote {
                if multiple {
                    let (lock, turn) = &*seen;
                    let mut state = lock.lock().unwrap();
                    state.0 += 1;
                    state.1 = state.1.max(state.0);
                    turn.notify_all();
                    if parallel {
                        let (state, waited) = turn
                            .wait_timeout_while(
                                state,
                                std::time::Duration::from_secs(60),
                                |state| state.1 < LANES_SEEN,
                            )
                            .unwrap();
                        assert!(
                            !waited.timed_out(),
                            "four repository refreshes never ran at once"
                        );
                        drop(state);
                    }
                }
                for arg in &mut command.args {
                    if arg == "origin" {
                        *arg = bare.as_os_str().to_owned();
                    }
                }
            } else if command.args.iter().any(|arg| arg == "fetch") {
                for arg in &mut command.args {
                    if arg == "origin" {
                        *arg = bare.as_os_str().to_owned();
                    }
                }
                command
                    .args
                    .push("+refs/heads/*:refs/remotes/origin/*".into());
            }
            let ran = if remote && case == "parallel offline" {
                let mut ran = Ran::exit(128, "");
                ran.stderr = b"Could not resolve host: fixture.invalid".to_vec();
                ran
            } else {
                Live::new().run_complete_stdout(&command).unwrap()
            };
            if remote && multiple {
                seen.0.lock().unwrap().0 -= 1;
            }
            if remote && case == "parallel stopped" {
                sys.file(
                    inventory.to_str().unwrap(),
                    b"source withdrawn during remote read",
                );
            }
            if remote && case == "parallel rebound" {
                let subject = work.join(".git/objects");
                let mut identity = Live::new().file_id(&subject).unwrap();
                identity.ino = identity.ino.wrapping_add(1);
                sys.file_id_of(&subject, identity);
            }
            if case == "late retargeted" && command.args.iter().any(|arg| arg == "--symref") {
                let live = Live::new();
                let changed = live
                    .run_complete_stdout(&crate::checkout::git(&live, &who, Some(work)).args([
                        "config",
                        "branch.main.remote",
                        "other",
                    ]))
                    .unwrap();
                assert!(changed.success());
            }
            if case == "late partial" && command.args.iter().any(|arg| arg == "--symref") {
                let live = Live::new();
                let name = work.file_name().unwrap().to_str().unwrap();
                for setting in [
                    ["extensions.partialClone".to_owned(), "origin".to_owned()],
                    [
                        format!("url.{}.insteadOf", bare.display()),
                        format!("https://github.com/{}/{name}.git", who.name),
                    ],
                ] {
                    let changed = live
                        .run_complete_stdout(
                            &crate::checkout::git(&live, &who, Some(work))
                                .arg("config")
                                .args(setting),
                        )
                        .unwrap();
                    assert!(changed.success());
                }
            }
            if case == "changed source" && command.args.iter().any(|arg| arg == "--symref") {
                sys.file(
                    inventory.to_str().unwrap(),
                    b"source withdrawn during remote read",
                );
            }
            if matches!(case, "late recorded" | "unchanged develop")
                && command.args.iter().any(|arg| arg == "--symref")
            {
                let bytes = if case == "late recorded" {
                    b"[develop]\ncheckouts=['~/work']\n".as_slice()
                } else {
                    b"[develop]\ncheckouts=[]\n".as_slice()
                };
                sys.file(profile.to_str().unwrap(), bytes).stat_of(
                    profile.to_str().unwrap(),
                    Stat {
                        uid,
                        gid,
                        len: bytes.len() as u64,
                        ..stat(false, 0o600)
                    },
                );
            }
            Some(ran)
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&repos.who));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        let check = run_captured(
            &sys,
            &ctx,
            Mode::Plan,
            RepositoryWork::Refresh,
            inputs.clone(),
            Box::new(|| Ok(true)),
        )
        .unwrap();
        assert_eq!(
            stage(&check).outcome,
            crate::model::Outcome::Ok,
            "{check:?}"
        );
        assert_eq!(repos.head(&repos.work), before);
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("--symref") && !call.contains(" fetch "))
        );

        // A checkout-only repair wake uses the same real runner but never forces
        // fresh remote heads of already acquired parents.
        let repaired = run_captured(
            &sys,
            &ctx,
            Mode::Apply,
            RepositoryWork::Repair,
            inputs.clone(),
            Box::new(|| Ok(true)),
        )
        .unwrap();
        assert_eq!(
            stage(&repaired).outcome,
            crate::model::Outcome::Ok,
            "{case}: {repaired:?}"
        );
        assert_eq!(repos.head(&repos.work), before);
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("--symref") && !call.contains(" fetch "))
        );

        let applied = run_captured(
            &sys,
            &ctx,
            Mode::Apply,
            RepositoryWork::Refresh,
            inputs,
            Box::new(|| Ok(true)),
        )
        .unwrap();
        if matches!(
            case,
            "parallel stopped" | "parallel rebound" | "parallel offline"
        ) {
            assert_eq!(
                stage(&applied).outcome,
                crate::model::Outcome::Failed,
                "{case}: {applied:?}"
            );
            assert_eq!(gate.0.lock().unwrap().1, LANES_SEEN);
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains("--symref"))
                    .count(),
                LANES_SEEN,
                "no queued repository starts after the batch stops"
            );
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains(" fetch ") && !call.contains(" merge "))
            );
            for path in &works {
                assert_eq!(repos.head(path), before);
            }
            continue;
        }
        if matches!(
            case,
            "changed source" | "late recorded" | "late retargeted" | "late partial"
        ) {
            assert_eq!(
                stage(&applied).outcome,
                crate::model::Outcome::Failed,
                "{applied:?}"
            );
            assert!(sys.calls().iter().any(|call| call.contains("--symref")));
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains(" fetch ") && !call.contains(" merge "))
            );
            assert_eq!(repos.head(&repos.work), before);
            if case == "late partial" && honours_no_lazy_fetch {
                let promisor: Vec<_> = std::fs::read_dir(repos.work.join(".git/objects/pack"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|entry| entry.path().extension() == Some("promisor".as_ref()))
                    .map(|entry| entry.path())
                    .collect();
                assert_eq!(
                    promisor,
                    Vec::<std::path::PathBuf>::new(),
                    "a read fetched lazily"
                );
            }
            continue;
        }
        assert_eq!(
            stage(&applied).outcome,
            crate::model::Outcome::Ok,
            "{applied:?}"
        );
        assert_eq!(
            repos.head(&repos.work),
            if matches!(case, "clean" | "old git" | "unchanged develop") || multiple {
                tip.clone()
            } else {
                before.clone()
            },
            "{case}: default Apply must refresh acquired clean code"
        );
        if case.starts_with("old git") {
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("--no-lazy-fetch")),
                "{case}: {:?}",
                sys.calls()
            );
        }
        if multiple {
            assert_eq!(
                gate.0.lock().unwrap().1,
                if parallel { LANES_SEEN } else { 1 }
            );
            for path in &works {
                assert_eq!(repos.head(path), tip);
                assert!(repos.status(path).is_empty());
            }
        }
        if !matches!(case, "clean" | "old git" | "unchanged develop") && !multiple {
            assert!(
                sys.calls().iter().all(|call| !call.contains("--symref")
                    && !call.contains(" fetch ")
                    && !call.contains(" merge ")),
                "{case}: {:?}",
                sys.calls()
            );
        }
        if case == "dirty" {
            assert_eq!(
                std::fs::read(repos.work.join("README")).unwrap(),
                b"preserved edits\n"
            );
        } else {
            assert!(repos.status(&repos.work).is_empty());
        }
    }
}

#[test]
fn node_and_subscriber_share_a_source_refresh_wave_using_the_alignment_record() {
    use super::super::RepositoryRefreshCursor;
    use crate::port::Live;
    use crate::sync::repository_refresh::{Execution, RepositoryPass, observe};
    use aterm_secrets::converge::{RefreshRequest, RepositoryRequest};
    // The fake models this host and selects its native git, which the real runs use.
    let git = crate::items::github::need_native_git();

    let live = Live::new();
    let repos = crate::checkout::tests::Repos::new(&live, "native-shared-refresh");
    let first = repos.head(&repos.work);
    repos.g(
        &repos.work,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("https://github.com/{}/work.git", repos.who.name),
        ],
    );
    let second = repos.advance("second.txt", "source two\n");
    assert_eq!(repos.head(&repos.work), first);
    assert_eq!(
        repos.g(&repos.work, &["rev-parse", "refs/remotes/origin/main"]),
        first
    );

    let mut sys = Fake::root();
    sys.euid = repos.who.uid;
    sys.linux = !live.macos();
    sys.logged_in(&repos.who)
        .stat_of(
            &repos.who.home,
            Stat {
                uid: repos.who.uid,
                gid: repos.who.gid,
                ..stat(true, 0o700)
            },
        )
        .executable(git, b"selected fixture git")
        .stat_of(
            repos.work.to_str().unwrap(),
            Stat {
                uid: repos.who.uid,
                gid: repos.who.gid,
                ..stat(true, 0o700)
            },
        );
    for path in [
        repos.work.join(".git"),
        repos.work.join(".git/objects"),
        repos.work.join(".git/index"),
    ] {
        sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
    }
    script::launcher(&sys, &repos.who);
    owned_source(&sys, &repos.who);
    metadata(
        &sys,
        &repos.who,
        aterm_json::json!([repository(&repos.who.name, "work", 1)]),
    );
    script::lookups_hold(&sys, &repos.who, GITHUB, &repos.who.name, TOKEN);
    script::alike(&sys, &repos.who);
    let work = repos.work.clone();
    let bare = repos.origin.clone();
    sys.simulate(move |_, command| {
        if !command.args.iter().any(|arg| arg == work.as_os_str()) {
            return None;
        }
        let mut command = command.clone();
        let fetching = command.args.iter().any(|arg| arg == "fetch");
        if fetching || command.args.iter().any(|arg| arg == "--symref") {
            for arg in &mut command.args {
                if arg == "origin" {
                    *arg = bare.as_os_str().to_owned();
                }
            }
            if fetching {
                command
                    .args
                    .push("+refs/heads/*:refs/remotes/origin/*".into());
            }
        }
        Some(Live::new().run_complete_stdout(&command).unwrap())
    });

    let state = aterm_tempfile::tempdir().unwrap();
    let now = sys.now().saturating_mul(1_000);
    assert!(crate::sync::read_record(state.path()).is_none());
    // Receipt samples precede both producers' initial actual source readings.
    let node_before = observe(state.path(), now);
    let subscriber_before = observe(state.path(), now);
    let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&repos.who));
    let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    let scope = inputs.refresh_scope(&sys).unwrap();
    let initial_mark = super::super::repository_input_mark(&sys).unwrap();
    assert_eq!(
        initial_mark.refresh_scope(),
        Some(scope),
        "the producer and the effect runner derive the same production scope"
    );
    let mut node_cursor = RepositoryRefreshCursor::new(node_before, Some(&initial_mark));
    let mut subscriber_cursor =
        RepositoryRefreshCursor::new(subscriber_before, Some(&initial_mark));
    let Some(RepositoryRequest::Refresh(node)) =
        node_cursor.observe(Some(&initial_mark), node_before, None)
    else {
        panic!("the node's first actual source ensures freshness");
    };
    let Some(RepositoryRequest::Refresh(subscriber)) =
        subscriber_cursor.observe(Some(&initial_mark), subscriber_before, None)
    else {
        panic!("the subscriber's first actual source ensures freshness");
    };
    let apply = |inputs: InitialRepositoryInputs, request: RefreshRequest| {
        let access = RepositoryPass::open(state.path(), now).unwrap();
        let pass = access
            .pass
            .expect("the prior owner released the file lease");
        let rows = run_captured(
            &sys,
            &ctx,
            Mode::Apply,
            RepositoryWork::Refresh,
            inputs.with_repository_execution(Some(Execution {
                request: RepositoryRequest::Refresh(request),
                pass: Arc::clone(&pass),
            })),
            Box::new(|| Ok(true)),
        )
        .unwrap();
        (rows, pass.record())
    };
    let network = || {
        let calls = sys.calls();
        (
            calls
                .iter()
                .filter(|call| call.contains("--symref"))
                .count(),
            calls.iter().filter(|call| call.contains(" fetch ")).count(),
        )
    };
    let (rows, empty) = apply(inputs.clone(), node);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(repos.head(&repos.work), second);
    assert_eq!(network(), (1, 1));
    assert_eq!(empty.build, "");
    assert_eq!(empty.spec, 0);
    assert_eq!(empty.attempted_ms, 0);
    assert_eq!(empty.converged_ms, None);
    assert!(empty.builds.is_empty() && empty.routes.is_empty() && empty.rows.is_empty());
    assert_eq!(empty.base, None);
    assert_eq!(
        crate::sync::due(Some(&empty), "future-build", &[], now),
        crate::sync::Due::NewBuild
    );

    // Repository-only completion cannot stamp the generic pass's build, routes or base.
    let expected = {
        let pass = RepositoryPass::open(state.path(), now)
            .unwrap()
            .pass
            .unwrap();
        let generic = crate::sync::Record {
            build: "kept-build".into(),
            spec: crate::sync::SPEC,
            builds: vec!["earlier-build".into()],
            attempted_ms: now - 1_000,
            converged_ms: Some(now - 500),
            routes: vec!["192.0.2.1".into()],
            rows: vec![crate::sync::RecordRow {
                id: "kept-row".into(),
                outcome: crate::model::Outcome::Ok,
                why: Some("kept result".into()),
            }],
            base: Some(crate::sync::BaseRecord {
                build: "kept-base".into(),
                pending: vec!["kept-item".into()],
            }),
            repositories: None,
        };
        pass.record_pass(generic).unwrap();
        pass.record()
    };
    let (rows, shared) = apply(inputs.clone(), subscriber);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(repos.head(&repos.work), second);
    assert_eq!(
        network(),
        (1, 1),
        "the follower performs no second remote wave"
    );
    assert_eq!(shared, expected);

    let latest = observe(state.path(), now);
    assert!(
        node_cursor
            .observe(Some(&initial_mark), latest, None)
            .is_none()
    );
    assert!(
        subscriber_cursor
            .observe(Some(&initial_mark), latest, None)
            .is_none()
    );
    let forced = RefreshRequest::after(Some(scope), latest.version);
    let third = repos.advance("third.txt", "source three\n");
    let (rows, refreshed) = apply(inputs.clone(), forced);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(repos.head(&repos.work), third);
    assert_eq!(
        network(),
        (2, 2),
        "a later explicit boundary refreshes the same source again"
    );
    let generic = |mut record: crate::sync::Record| {
        record.repositories = None;
        record
    };
    assert_eq!(generic(refreshed.clone()), generic(expected.clone()));
    assert_eq!(
        super::super::repository_input_mark(&sys)
            .unwrap()
            .refresh_scope(),
        Some(scope)
    );

    let before_change = observe(state.path(), now);
    metadata(
        &sys,
        &repos.who,
        aterm_json::json!([repository(&repos.who.name, "work", 2)]),
    );
    let changed = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    let changed_scope = changed.refresh_scope(&sys).unwrap();
    assert_ne!(changed_scope, scope);
    let changed_mark = super::super::repository_input_mark(&sys).unwrap();
    assert_eq!(changed_mark.refresh_scope(), Some(changed_scope));
    let fourth = repos.advance("fourth.txt", "source four\n");
    let (rows, refused) = apply(changed.clone(), forced);
    assert_eq!(
        stage(&rows).outcome,
        crate::model::Outcome::Failed,
        "{rows:?}"
    );
    assert_eq!(repos.head(&repos.work), third);
    assert_eq!(
        network(),
        (2, 2),
        "an old request cannot refresh a new admitted source"
    );
    assert_eq!(refused, refreshed);
    let Some(RepositoryRequest::Refresh(changed_request)) = node_cursor.observe(
        Some(&changed_mark),
        before_change,
        Some(RepositoryWork::Refresh),
    ) else {
        panic!("the node observes the actual source transition");
    };
    // A→B→A before any admission still owes a new generation when its bytes return.
    metadata(
        &sys,
        &repos.who,
        aterm_json::json!([repository(&repos.who.name, "work", 1)]),
    );
    let returned_mark = super::super::repository_input_mark(&sys).unwrap();
    let Some(RepositoryRequest::Refresh(returned)) = node_cursor.observe(
        Some(&returned_mark),
        before_change,
        Some(RepositoryWork::Refresh),
    ) else {
        panic!("returning to the earlier bytes retains the transition");
    };
    let unadmitted = changed_request.merge(returned);
    assert_eq!(unadmitted.scope, Some(scope));
    assert!(
        !before_change.fulfills(unadmitted),
        "ABA cannot inherit the old A fulfillment"
    );
    metadata(
        &sys,
        &repos.who,
        aterm_json::json!([repository(&repos.who.name, "work", 2)]),
    );
    let final_mark = super::super::repository_input_mark(&sys).unwrap();
    let Some(RepositoryRequest::Refresh(final_request)) = node_cursor.observe(
        Some(&final_mark),
        before_change,
        Some(RepositoryWork::Refresh),
    ) else {
        panic!("the final B source is the desired generation");
    };
    let changed_request = unadmitted.merge(final_request);
    let (rows, changed_record) = apply(changed.clone(), changed_request);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(repos.head(&repos.work), fourth);
    assert_eq!(network(), (3, 3));
    assert!(observe(state.path(), now).fulfills(changed_request));

    // This producer sees B only after the node already fulfilled B. Its old actual
    // source's epoch must bind the request, not the just-read B receipt's newer epoch.
    let after_change = observe(state.path(), now);
    let Some(RepositoryRequest::Refresh(delayed)) = subscriber_cursor.observe(
        Some(&final_mark),
        after_change,
        Some(RepositoryWork::Refresh),
    ) else {
        panic!("the delayed subscriber also observes the actual transition");
    };
    assert_eq!(delayed, changed_request);
    assert!(delayed.base_input < after_change.version.input);
    let (rows, delayed_record) = apply(changed, delayed);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(repos.head(&repos.work), fourth);
    assert_eq!(
        network(),
        (3, 3),
        "the delayed actual source observer shares the completed B wave"
    );
    assert_eq!(delayed_record, changed_record);
    assert_eq!(generic(changed_record), generic(expected.clone()));

    // A semantic develop selection can move A→B→A while this observer's actual
    // source stays A. A successful A sample against B's newer receipt owes A now
    // and raises the floor for a subsequent B transition.
    let profile = Path::new(&repos.who.home).join(".config/aterm/init.toml");
    let set_develop = |checkouts: &str| {
        let bytes = format!("[develop]\ncheckouts={checkouts}\n");
        sys.file(profile.to_str().unwrap(), bytes.as_bytes())
            .stat_of(
                profile.to_str().unwrap(),
                Stat {
                    uid: repos.who.uid,
                    gid: repos.who.gid,
                    len: bytes.len() as u64,
                    ..stat(false, 0o600)
                },
            );
    };
    let before_semantic = observe(state.path(), now);
    set_develop("[]");
    let semantic_a = super::super::repository_input_mark(&sys).unwrap();
    assert_eq!(semantic_a.refresh_scope(), Some(changed_scope));
    let mut late = RepositoryRefreshCursor::new(before_semantic, Some(&semantic_a));
    let Some(RepositoryRequest::Refresh(initial)) =
        late.observe(Some(&semantic_a), before_semantic, None)
    else {
        panic!("the late actor first ensures the actual A generation");
    };
    assert!(before_semantic.fulfills(initial));
    assert!(
        node_cursor
            .observe(Some(&semantic_a), before_semantic, None)
            .is_none()
    );

    set_develop("['~/parked']");
    let semantic_b = super::super::repository_input_mark(&sys).unwrap();
    let semantic_inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
    assert_eq!(
        semantic_b.refresh_scope(),
        Some(semantic_inputs.refresh_scope(&sys).unwrap())
    );
    assert_ne!(semantic_b.refresh_scope(), semantic_a.refresh_scope());
    let Some(RepositoryRequest::Refresh(winner)) = node_cursor.observe(
        Some(&semantic_b),
        before_semantic,
        Some(RepositoryWork::Refresh),
    ) else {
        panic!("the node refreshes the new semantic selection");
    };
    let before_network = network();
    let (rows, _) = apply(semantic_inputs.clone(), winner);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(network().0, before_network.0 + 1);
    let winner_receipt = observe(state.path(), now);
    assert!(winner_receipt.fulfills(winner));

    set_develop("[]");
    let still_a = super::super::repository_input_mark(&sys).unwrap();
    assert_eq!(still_a.refresh_scope(), semantic_a.refresh_scope());
    let Some(RepositoryRequest::Refresh(missed_a)) =
        late.observe(Some(&still_a), winner_receipt, None)
    else {
        panic!("unchanged actual A against a newer foreign B receipt owes A immediately");
    };
    assert_eq!(missed_a.scope, semantic_a.refresh_scope());
    assert_eq!(missed_a.base_input, winner_receipt.version.input);
    set_develop("['~/parked']");
    let back_to_b = super::super::repository_input_mark(&sys).unwrap();
    let Some(RepositoryRequest::Refresh(new_b)) = late.observe(
        Some(&back_to_b),
        winner_receipt,
        Some(RepositoryWork::Refresh),
    ) else {
        panic!("the later B transition retains the newer receipt floor");
    };
    let requested = missed_a.merge(new_b);
    assert!(
        !winner_receipt.fulfills(requested),
        "the previous B wave cannot acknowledge a later A→B transition"
    );
    let before_network = network();
    let (rows, semantic_record) = apply(semantic_inputs.clone(), requested);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(network().0, before_network.0 + 1);
    assert!(observe(state.path(), now).fulfills(requested));
    assert_eq!(generic(semantic_record), generic(expected));

    // An owned record reset cannot make a retained input/attempt fence require
    // repeated remote waves merely to count back up to its old epochs.
    let before_reset = observe(state.path(), now);
    let retained_scope = semantic_inputs.refresh_scope(&sys).unwrap();
    let retained = RefreshRequest::inputs(retained_scope, before_reset.version).merge(
        RefreshRequest::after(Some(retained_scope), before_reset.version),
    );
    {
        let _fence = RepositoryPass::open(state.path(), now)
            .unwrap()
            .pass
            .unwrap();
        std::fs::remove_file(
            state
                .path()
                .join(aterm_converge::FileJournal::DIR)
                .join(crate::sync::RECORD),
        )
        .unwrap();
    }
    assert!(crate::sync::read_record(state.path()).is_none());
    let fifth = repos.advance("fifth.txt", "source five\n");
    let before_network = network();
    let (rows, reset_record) = apply(semantic_inputs.clone(), retained);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(repos.head(&repos.work), fifth);
    assert_eq!(network(), (before_network.0 + 1, before_network.1 + 1));
    let reset_receipt = observe(state.path(), now);
    assert!(
        reset_receipt.fulfills(retained),
        "one wave exceeds both retained epoch fences after reset"
    );
    assert!(reset_receipt.version.input > retained.base_input);
    assert!(reset_receipt.version.attempt > retained.after_attempt.unwrap());
    assert_eq!(generic(reset_record.clone()), generic(empty));
    assert_eq!(
        crate::sync::due(Some(&reset_record), "kept-build", &[], now),
        crate::sync::Due::NewBuild
    );
    let before_network = network();
    let (rows, reset_follower) = apply(semantic_inputs, retained);
    assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
    assert_eq!(network(), before_network);
    assert_eq!(reset_follower, reset_record);
}

#[test]
fn private_inventory_branch_history_does_not_strand_new_checkouts() {
    for cached in [aterm_json::json!("old-default"), aterm_json::Value::Null] {
        let (sys, item) = listing(&["new".into()], &[]);
        let mut value = repository("alice", "new", 1);
        value
            .as_object_mut()
            .unwrap()
            .insert("default_branch".into(), cached);
        metadata(&sys, &item.user, aterm_json::json!([value]));
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Changed,
            "{rows:?}"
        );
        assert_eq!(
            Reading::of(stage(&rows)),
            Some(Reading::Repositories {
                present: 1,
                count: 1,
                signed_in: true
            })
        );
        assert!(
            sys.stat(&Path::new(&item.user.home).join("new"))
                .unwrap()
                .dir
        );
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains(" clone "))
                .count(),
            1
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }
}

/// How many of `calls` ran git as alice in `path` with `args`.
fn ran_in(calls: &[String], path: &Path, args: &str) -> usize {
    let at = format!("-C {} {args}", path.display());
    calls.iter().filter(|call| call.ends_with(&at)).count()
}

const TOP_LEVEL: &str =
    "rev-parse --is-inside-work-tree --show-prefix --show-toplevel --git-path index";

/// One whole pass over 50 repositories already in `~` and 10 to clone: private metadata is
/// captured once, and completion rechecks all worktrees locally after cloning. Each
/// missing repository has one remote inventory and one clone; local route admission is
/// repeated before network operations.
#[test]
fn a_whole_pass_reuses_private_inventory_but_rechecks_all_local_worktrees() {
    let (present, missing) = (names("repo", 50), names("new", 10));
    let listed: Vec<String> = present.iter().chain(&missing).cloned().collect();
    let (sys, item) = listing(&listed, &present);
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let rows = run(&sys, &context, Mode::Apply).unwrap();
    assert_eq!(
        stage(&rows).outcome,
        crate::model::Outcome::Changed,
        "{rows:?}"
    );
    assert_eq!(
        stage(&rows).to.as_deref(),
        Some("60 matching worktrees present; aterm's GitHub route hands back the synced token")
    );
    let calls = sys.calls();
    let count = |what: &str| calls.iter().filter(|call| call.ends_with(what)).count();
    assert_eq!(count(" agh GET /user"), 0, "{calls:?}");
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.contains(" agh GET /user/repos"))
            .count(),
        0,
        "repository inventory is internal vault metadata"
    );
    let home = Path::new(&item.user.home);
    for name in &present {
        let path = home.join(name);
        assert_eq!(ran_in(&calls, &path, TOP_LEVEL), 2, "{name}");
        assert_eq!(
            ran_in(&calls, &path, "config --get-all remote.origin.url"),
            2,
            "{name}"
        );
    }
    for name in &missing {
        assert_eq!(ran_in(&calls, &home.join(name), TOP_LEVEL), 1, "{name}");
    }
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.contains("ls-remote --symref"))
            .count(),
        missing.len(),
        "{calls:#?}"
    );
    assert_eq!(
        calls.iter().filter(|call| call.contains(" clone ")).count(),
        missing.len(),
        "{calls:#?}"
    );
}

/// The native stage's guards meet the installers: a guard that stops the rest once its clone
/// ran its installer fails the row with that outcome after why, and a switch turned off
/// before the read-back answers not applicable with it. NEGATIVE CONTROL: the switch read
/// before the outcomes are taken drops the line from that answer.
#[test]
fn a_guard_or_switch_after_the_clone_still_says_what_its_installer_did() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const INSTALLED: &str = "Trust git hooks: installed (core.hooksPath = scripts/hooks)";
    let says = format!("$HOME/trust: ran its {}: {INSTALLED:?}", repos::HOOK_INSTALLER);
    for stop in [true, false] {
        let (sys, item) = listing(&["trust".to_owned()], &[]);
        let home = Path::new(&item.user.home);
        let (staged, clone) = repos::tests::clone_key(&sys, &item.user, "trust", 0);
        sys.on_stat(&clone, &staged.join("scripts"), stat(true, 0o755));
        sys.on_write(
            &clone,
            &staged.join(repos::HOOK_INSTALLER),
            b"#!/usr/bin/env bash\n",
        );
        let installer = repos::installer_command(&sys, &item.user, &home.join("trust")).display();
        sys.on(&installer, Ran::ok(&format!("{INSTALLED}\n")));
        let ran = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&ran);
        sys.simulate(move |_, command| {
            if command.display() == installer {
                seen.store(true, Ordering::SeqCst);
            }
            None
        });
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
        let authorized = || {
            if stop && ran.load(Ordering::SeqCst) {
                Err("late Stop".to_owned())
            } else {
                Ok(())
            }
        };
        let applied = item.apply_guarded(&sys, &context, &authorized);
        assert!(ran.load(Ordering::SeqCst), "the installer ran");
        if stop {
            let why = applied.unwrap_err();
            assert_eq!(why, format!("late Stop; {says}"));
            continue;
        }
        applied.unwrap();
        sys.write_user_file(
            &item.user,
            &home.join(".config/aterm/init.toml"),
            b"[github]\nenabled = false\n",
            0o600,
        )
        .unwrap();
        match item.probe(&sys, &context) {
            Probe::NotApplicable { why } => assert_eq!(
                why,
                format!("repository setup is disabled in init.toml's [github] table; {says}")
            ),
            other => panic!("{other:?}"),
        }
    }
}

/// The stage's row says what a clone it made did with the hook installer it carries: the
/// read-back's line ends with the installer's own first line, and a failing installer leaves
/// the stage changed, its clone in place. CONTROL: a clone with no installer adds nothing.
#[test]
fn the_stage_row_says_what_a_new_clones_hook_installer_did() {
    const INSTALLED: &str = "Trust git hooks: installed (core.hooksPath = scripts/hooks)";
    let present = "1 matching worktrees present; aterm's GitHub route hands back the synced \
                   token";
    for (carries, reply, says) in [
        (
            true,
            Ran::ok(&format!("{INSTALLED}\n")),
            format!(
                "{present}; $HOME/trust: ran its {}: {INSTALLED:?}",
                repos::HOOK_INSTALLER
            ),
        ),
        (
            true,
            Ran::exit(1, "ERROR: refused\n"),
            format!(
                "{present}; $HOME/trust: its {} failed (exit 1: \"ERROR: refused\"); the clone \
                 stands",
                repos::HOOK_INSTALLER
            ),
        ),
        (false, Ran::ok(INSTALLED), present.to_owned()),
    ] {
        let (sys, item) = listing(&["trust".to_owned()], &[]);
        let home = Path::new(&item.user.home);
        if carries {
            let (staged, clone) = repos::tests::clone_key(&sys, &item.user, "trust", 0);
            let scripts = staged.join("scripts");
            sys.on_stat(&clone, &scripts, stat(true, 0o755));
            sys.on_write(
                &clone,
                &staged.join(repos::HOOK_INSTALLER),
                b"#!/usr/bin/env bash\n",
            );
        }
        let installer = repos::installer_command(&sys, &item.user, &home.join("trust"));
        sys.on(&installer.display(), reply);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Changed,
            "{rows:?}"
        );
        assert_eq!(stage(&rows).to.as_deref(), Some(says.as_str()), "{rows:?}");
        assert_eq!(
            sys.calls().contains(&installer.display()),
            carries,
            "{:?}",
            sys.calls()
        );
    }
}

/// Up to four clones run at once: each of the first four waits, inside its clone, for the
/// others to start (a one-minute hang detector, never a latency budget), which a pass that
/// clones one at a time never lets happen; a fifth never runs beside them. No more staging
/// directories exist than clones running, so a pass cut short leaves at most four behind.
#[test]
fn four_clones_run_at_once() {
    use std::sync::{Arc, Condvar, Mutex};
    let missing = names("new", 6);
    let (sys, item) = listing(&missing, &[]);
    let gate = Arc::new((Mutex::new((0_usize, 0_usize)), Condvar::new()));
    let seen = Arc::clone(&gate);
    sys.simulate(move |fake, cmd| {
        if !cmd.display().contains(" clone --no-single-branch ") {
            return None;
        }
        let calls = fake.calls();
        let marked = |what: &str| calls.iter().filter(|call| call.starts_with(what)).count();
        let staged = marked("scratch allocated ") - marked("scratch closed ");
        assert!(staged <= LANES_SEEN, "{staged} staging directories at once");
        let (lock, turn) = &*seen;
        let mut state = lock.lock().unwrap();
        state.0 += 1;
        state.1 = state.1.max(state.0);
        turn.notify_all();
        let (mut state, waited) = turn
            .wait_timeout_while(state, std::time::Duration::from_secs(60), |state| {
                state.1 < LANES_SEEN
            })
            .unwrap();
        assert!(!waited.timed_out(), "four clones never ran at once");
        state.0 -= 1;
        None
    });
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    item.apply(&sys, &context).unwrap();
    assert_eq!(gate.0.lock().unwrap().1, LANES_SEEN);
    let home = Path::new(&item.user.home);
    for name in &missing {
        assert!(sys.stat(&home.join(name)).is_ok(), "{name}");
    }
}

/// The most clones a pass runs at once.
const LANES_SEEN: usize = 4;

/// A clone that no longer reads as the repository's own worktree there during read-back
/// (another origin took its place once it was
/// published) is still missing: the row fails instead of counting it present or dropping it.
#[test]
fn a_clone_the_read_back_cannot_verify_is_still_missing() {
    let (sys, item) = listing(&["new".to_owned()], &[]);
    let origin = format!(
        "-C {}/new config --get-all remote.origin.url",
        item.user.home
    );
    sys.simulate(move |_, cmd| {
        cmd.display()
            .ends_with(&origin)
            .then(|| Ran::ok("https://github.com/someone/new.git\n"))
    });
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let rows = run(&sys, &context, Mode::Apply).unwrap();
    assert_eq!(
        stage(&rows).outcome,
        crate::model::Outcome::Failed,
        "{rows:?}"
    );
    assert_eq!(
        Reading::of(stage(&rows)),
        Some(Reading::Repositories {
            present: 0,
            count: 1,
            signed_in: true,
        })
    );
    assert!(
        sys.calls()
            .iter()
            .any(|call| call.starts_with("rename-noreplace ")),
        "the clone was published before the read-back: {:?}",
        sys.calls()
    );
}

#[test]
fn readback_rechecks_preexisting_worktrees_and_dependencies_after_a_clone_batch() {
    use super::super::dependencies::tests::{PIN, existing};
    use std::sync::atomic::{AtomicBool, Ordering};
    for dependency in [false, true] {
        let names = ["parent".to_owned(), "new".to_owned()];
        let (sys, item) = listing(&names, &["parent".to_owned()]);
        let home = Path::new(&item.user.home);
        let parent = home.join("parent");
        let child = parent.join("vendor/library");
        if dependency {
            required_repository(&sys, &item.user, "parent", &["vendor/library"]);
            let mut required = repository("alice", "parent", 1);
            required.as_object_mut().unwrap().insert(
                "required_submodules".into(),
                aterm_json::json!(["vendor/library"]),
            );
            metadata(
                &sys,
                &item.user,
                aterm_json::json!([required, repository("alice", "new", 2),]),
            );
            sys.stat_of(parent.join("vendor").to_str().unwrap(), stat(true, 0o700));
            sys.stat_of(child.to_str().unwrap(), stat(true, 0o700));
            sys.file(child.join(".git").to_str().unwrap(), b"inert metadata")
                .stat_of(child.join(".git").to_str().unwrap(), stat(false, 0o600));
            existing(&sys, &item.user, &child, "library", PIN);
            super::super::dependencies::tests::download(
                &sys,
                &item.user,
                &parent,
                "vendor/library",
                1,
            );
        } else {
            repos::tests::script_clone_named(&sys, &item.user, "parent", 1, false);
        }
        let changed = AtomicBool::new(false);
        let removed = if dependency { child } else { parent };
        sys.simulate(move |sys, command| {
            if command.display().contains(" clone ") && !changed.swap(true, Ordering::SeqCst) {
                // Another local process removes an old checkout while a different
                // repository downloads. The credential and inventory do not change.
                if dependency {
                    sys.remove(&removed.join(".git")).unwrap();
                }
                sys.remove(&removed).unwrap();
            }
            None
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let source = super::super::token_read(&sys, &ctx).unwrap().unwrap().mark;
        let rows = run(&sys, &ctx, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Failed,
            "dependency={dependency}: {rows:?}"
        );
        assert_eq!(
            Reading::of(stage(&rows)),
            Some(Reading::Repositories {
                present: 1,
                count: 2,
                signed_in: true,
            }),
            "dependency={dependency}: {rows:?}"
        );
        assert_eq!(
            super::super::token_read(&sys, &ctx).unwrap().unwrap().mark,
            source
        );
        let resumed = run(&sys, &ctx, Mode::Apply).unwrap();
        assert_eq!(
            stage(&resumed).outcome,
            crate::model::Outcome::Changed,
            "dependency={dependency}: {resumed:?}"
        );
        assert_eq!(
            Reading::of(stage(&resumed)),
            Some(Reading::Repositories {
                present: 2,
                count: 2,
                signed_in: true,
            })
        );
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains(" clone "))
                .count(),
            2
        );
    }
}

/// Existing destinations stay untouched while the others clone. They remain part of the
/// expected count and keep the stage unfinished; preserving a folder never completes it.
#[test]
fn blocked_destinations_remain_unfinished_while_the_other_repositories_clone() {
    let listed: Vec<String> = ["Documents", "infra", "new"].map(str::to_owned).to_vec();
    let (sys, item) = listing_with(&listed, &[], &["infra"]);
    let home = Path::new(&item.user.home);
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let rows = run(&sys, &context, Mode::Apply).unwrap();
    assert_eq!(
        stage(&rows).outcome,
        crate::model::Outcome::Failed,
        "{rows:?}"
    );
    assert_eq!(
        stage(&rows)
            .explain
            .as_ref()
            .map(|explain| explain.why.as_str()),
        Some(
            "1 of 3 repositories ready; preserved work or dependency metadata blocks completion: \
             ~/Documents is a standard home folder; left untouched; \
             ~/infra has a different or credential-bearing GitHub origin; left untouched"
        )
    );
    assert!(sys.stat(&home.join("new")).is_ok());
    let calls = sys.calls();
    let documents = home.join("Documents");
    assert!(
        calls
            .iter()
            .all(|call| !call.contains(&documents.display().to_string())),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| !call.contains("rename-noreplace") || call.ends_with("/new")),
        "{calls:?}"
    );
}

#[test]
fn an_entirely_blocked_inventory_never_claims_zero_repositories_are_complete() {
    let listed = names("occupied", 53);
    let theirs: Vec<&str> = listed.iter().map(String::as_str).collect();
    let (sys, item) = listing_with(&listed, &[], &theirs);
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    assert!(matches!(item.probe(&sys, &context), Probe::Person { .. }));
    assert_eq!(
        item.detail(&sys, &context),
        Some(
            Reading::Repositories {
                present: 0,
                count: 53,
                signed_in: true
            }
            .token()
        )
    );
    assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
}

/// `aterm init`'s default stage on a Mac the vault wrote no GitHub token to asks nobody
/// anything: its row is not applicable, and neither git nor GitHub is asked. An explicit
/// request with a token and selected repository waits for Git's automatic installation
/// on that Mac (the control).
#[test]
fn init_s_default_stage_without_a_token_is_not_applicable_and_asks_nothing() {
    let (sys, bare) = bare();
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&bare.user));
    match item(&bare.user, None, true, RepositoryWork::Refresh, None, None).probe(&sys, &context) {
        Probe::NotApplicable { why } => {
            assert!(why.contains("~/.secrets/gh_access_token"), "{why}");
            assert!(why.contains("on this Mac yet"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        sys.calls().iter().all(
            |call| !call.contains(" agh GET ") && !call.contains(crate::items::github::CLT_GIT)
        ),
        "{:?}",
        sys.calls()
    );
    source(&sys, "/Users//alice/.secrets/gh_access_token");
    metadata(
        &sys,
        &bare.user,
        aterm_json::json!([repository("alice", "ready", 1)]),
    );
    assert!(matches!(
        item(&bare.user, None, false, RepositoryWork::Refresh, None, None).probe(&sys, &context),
        Probe::Blocked { .. }
    ));
    // On Linux the row names the machine, not a Mac.
    let (mut linux, _) = self::bare();
    linux.linux = true;
    match item(&bare.user, None, true, RepositoryWork::Refresh, None, None).probe(&linux, &context)
    {
        Probe::NotApplicable { why } => assert!(why.contains("on this machine yet"), "{why}"),
        other => panic!("{other:?}"),
    }
    // So does an init.toml `[github]` account's step, whose token file is not there yet
    // (with the distribution's git there, so the step reaches the token): a blocked
    // operation, never a person's step.
    linux.executable(crate::items::github::SYSTEM_GIT, b"synthetic git");
    match item(&bare.user, None, false, RepositoryWork::Refresh, None, None).probe(&linux, &context)
    {
        Probe::Blocked { why } => assert!(why.contains("is not on this machine yet"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// The default set is the account's own repositories that are neither forks nor archived;
/// an init.toml `[github]` owner keeps them all (the control).
#[test]
fn forks_and_archived_repositories_are_left_out_unless_init_toml_names_the_owner() {
    let mut listed = vec![
        repository("alice", "fork", 1),
        repository("alice", "kept", 2),
        repository("alice", "old", 3),
    ];
    for (index, key) in [(0, "fork"), (2, "archived")] {
        listed[index]
            .as_object_mut()
            .unwrap()
            .insert(key.into(), aterm_json::Value::Bool(true));
    }
    let names = |item: &NativeRepositories, sys: &Fake| -> Vec<String> {
        item.plan(sys)
            .unwrap()
            .repositories
            .into_iter()
            .map(|repo| repo.name)
            .collect()
    };
    let (sys, item) = fixture();
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::Value::Array(listed.clone()),
    );
    assert_eq!(names(&item, &sys), ["kept"]);

    let (sys, item) = fixture();
    let profile = "/Users//alice/.config/aterm/init.toml";
    let bytes = b"[github]\naccount = 'alice'\n";
    sys.file(profile, bytes).stat_of(
        profile,
        Stat {
            len: bytes.len() as u64,
            ..stat(false, 0o600)
        },
    );
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::Value::Array(listed),
    );
    assert_eq!(names(&item, &sys), ["fork", "kept", "old"]);
}

/// The keychain block is replaced only once the engine journaled the commands that put it
/// back, the settings exactly as they were written. A helper of someone else's that git
/// already uses (git's `store`, here, which reads no keychain) stays as it is, and the clones
/// carry aterm's route past it, with the person's own URL rewrite (the control).
#[test]
fn the_keychain_block_is_journaled_before_it_is_replaced_and_someone_elses_helper_stays() {
    let journaled = |sys: &Fake| -> Vec<aterm_json::Value> {
        sys.written(Fake::JOURNAL)
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .unwrap_or_default()
            .lines()
            .map(|line| aterm_json::from_str(line).unwrap())
            .collect()
    };
    let (sys, item) = fixture();
    answer(
        &sys,
        &item.user,
        &format!("{ACCESSIBLE}&page=1"),
        aterm_json::json!([]),
    );
    let writes = keychain_block_then_vault(&sys, &item.user);
    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
    let first = writes[0].clone();
    sys.simulate(move |fake, cmd| {
        if cmd.display() == first {
            assert!(
                fake.written(Fake::JOURNAL)
                    .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains("--add")),
                "the block was replaced before its revert was journaled"
            );
        }
        None
    });
    let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
    let rows = run(&sys, &context, Mode::Apply).unwrap();
    assert_eq!(rows[0].outcome, crate::model::Outcome::Changed, "{rows:?}");
    let lines = journaled(&sys);
    let revert = lines[0]["revert"].as_str().unwrap();
    assert!(
        revert.contains(
            "git config --file /Users//alice/.gitconfig --add \
             credential.https://github.com.helper ''; git config --file \
             /Users//alice/.gitconfig --add credential.https://github.com.helper osxkeychain; \
             git config --file /Users//alice/.gitconfig --add \
             credential.https://github.com.username alice; git config --file \
             /Users//alice/.gitconfig --add credential.https://github.com.usehttppath false \
             (puts back"
        ),
        "{revert}"
    );
    assert!(writes.iter().all(|write| sys.calls().contains(write)));

    // Control: a helper of the person's own for github.com (git's `store`, no keychain) is
    // someone else's. The one missing repository clones through aterm's route, an SSH
    // rewrite of the person's included.
    for rewritten in [false, true] {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([repository("alice", "new", 1)]),
        );
        super::repos::tests::script_clone_named_route(&sys, &item.user, "new", 0, false, rewritten);
        super::repos::tests::script_existing(
            &sys,
            &item.user,
            &Path::new(&item.user.home).join("new"),
            "new",
        );
        let helper = "credential.https://github.com.helper";
        sys.on(
            &script::helpers(&item.user),
            Ran::ok(&script::printed(&[
                (script::CLT_CONFIG, "credential.helper", "osxkeychain"),
                ("/Users//alice/.gitconfig", helper, ""),
                ("/Users//alice/.gitconfig", helper, "store"),
            ])),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::NotApplicable);
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Changed,
            "{rows:?}"
        );
        let now = stage(&rows).to.as_deref().unwrap();
        assert!(now.contains("hands back the synced token"), "{now}");
        assert!(!now.contains("store"), "{now}");
        let calls = sys.calls();
        assert!(
            calls.iter().all(|call| !call.contains("--global")
                && !call.contains("--unset-all")
                && !call.contains("credential approve")),
            "{calls:?}"
        );
        let clone = calls
            .iter()
            .find(|call| call.contains(" clone "))
            .expect("the clone ran");
        assert!(
            clone.contains("secrets git-credential") && clone.contains(helper),
            "{clone}"
        );
        assert_eq!(journaled(&sys)[0]["revert"], aterm_json::Value::Null);
    }
}
