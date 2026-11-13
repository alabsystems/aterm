// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::git_credential::script;
use crate::items::defaults::tests::{ctx, user};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, GithubResponse, Live, Ran, Stat};
use std::path::PathBuf;

mod clone_route_tests;
mod default_branch_tests;
mod hook_installer_tests;
mod staging_tests;

const TOKEN: &str = "fixture_synced_token_only";
/// alice's token file, as the vault leaves it.
const SOURCE: &str = "/Users//alice/.secrets/github";
const BASE: &str =
    "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc";
const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";

fn directory(uid: u32) -> Stat {
    Stat {
        uid,
        gid: 20,
        mode: 0o700,
        dir: true,
        symlink: false,
        len: 0,
    }
}

fn config() -> Github {
    Github {
        account: "alice".into(),
        owner: "alice".into(),
        token_file: SOURCE.into(),
        token_key: None,
    }
}

fn token() -> Secret {
    Secret::new(TOKEN.as_bytes()).unwrap()
}

fn fixture() -> (Fake, User, Github) {
    let mut sys = Fake::root();
    let who = user("alice", 501);
    sys.euid = who.uid;
    sys.stat_of(&who.home, directory(who.uid));
    sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
    let profile = "/Users//alice/.config/aterm/init.toml";
    let bytes = format!("[github]\naccount='alice'\nowner='alice'\ntoken_file='{SOURCE}'\n");
    sys.file(profile, bytes.as_bytes()).stat_of(
        profile,
        Stat {
            uid: who.uid,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: bytes.len() as u64,
        },
    );
    // Her token file, which aterm's route over that file hands back.
    sys.file(SOURCE, TOKEN.as_bytes()).stat_of(
        SOURCE,
        Stat {
            uid: who.uid,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: TOKEN.len() as u64,
        },
    );
    script::launcher(&sys, &who);
    let helper = config().helper(&sys, &who).expect("the launcher is there");
    script::hands_back(&sys, &who, &helper, TOKEN);
    sys.on_github_api(&who, "/user", json(r#"{"login":"alice"}"#));
    (sys, who, config())
}

fn value(name: &str, id: u64, private: bool) -> Value {
    aterm_json::json!({
        "id": id, "name": name, "full_name": format!("alice/{name}"),
        "owner": {"login": "alice"}, "private": private,
        "clone_url": format!("https://github.com/alice/{name}.git"),
        "default_branch": "main", "size": 0,
    })
}

fn json(body: &str) -> Result<GithubResponse, String> {
    Ok(GithubResponse {
        status: 200,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: body.as_bytes().to_vec(),
    })
}

fn page(values: Vec<Value>, links: Option<String>) -> Result<GithubResponse, String> {
    let mut answer = json(&aterm_json::to_string(&values).unwrap())?;
    answer
        .headers
        .extend(links.map(|links| ("Link".to_owned(), links)));
    Ok(answer)
}

#[test]
fn pagination_includes_private_empty_and_every_page_without_any_mutation() {
    let (sys, who, config) = fixture();
    let first = (0..30)
        .map(|i| value(&format!("repo{i:03}"), i + 1, i % 2 == 0))
        .collect();
    let links = format!(
        "<https://api.github.com{BASE}&page=2>; rel=\"next\", <https://api.github.com{BASE}&page=2>; rel=\"last\""
    );
    sys.on_github_api(&who, &format!("{BASE}&page=1"), page(first, Some(links)));
    let mut empty = value("zz-empty-private", 101, true);
    empty
        .as_object_mut()
        .unwrap()
        .insert("default_branch".into(), Value::Null);
    sys.on_github_api(&who, &format!("{BASE}&page=2"), page(vec![empty], None));
    let repos = enumerate(&sys, &who, &config, &token())
        .unwrap()
        .repositories;
    assert_eq!(repos.len(), 31);
    assert_eq!(repos.last().unwrap().default_branch, None);
    assert_eq!(sys.calls().len(), 2);
    assert!(
        sys.calls()
            .iter()
            .all(|call| call.contains(" agh GET ") && !call.contains("clone"))
    );

    let (sys, who, config) = fixture();
    sys.on_github_api(
        &who,
        &format!("{BASE}&page=1"),
        page(
            (0..30)
                .map(|i| value(&format!("repo{i:03}"), i + 1, false))
                .collect(),
            None,
        ),
    );
    sys.on_github_api(
        &who,
        &format!("{BASE}&page=2"),
        page(
            vec![],
            Some(format!(
                "<https://api.github.com{BASE}&page=1>; rel=\"prev\", <https://api.github.com{BASE}&page=1>; rel=\"last\""
            )),
        ),
    );
    assert_eq!(
        enumerate(&sys, &who, &config, &token())
            .unwrap()
            .repositories
            .len(),
        30
    );
}

#[test]
fn a_profile_changed_after_probe_refuses_the_prepared_repository_source() {
    let (sys, who, config) = fixture();
    sys.on_github_api(
        &who,
        &format!("{BASE}&page=1"),
        page(vec![value("new", 1, true)], None),
    );
    let ctx = ctx(std::slice::from_ref(&who));
    let item = item(&who, &config, Vec::new());
    assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
    let path = "/Users//alice/.config/aterm/init.toml";
    let bytes = format!("[github]\naccount='alice'\nowner='team'\ntoken_file='{SOURCE}'\n");
    sys.file(path, bytes.as_bytes()).stat_of(
        path,
        Stat {
            uid: who.uid,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: bytes.len() as u64,
        },
    );
    sys.on_github_api(&who, &format!("{BASE}&page=1"), page(Vec::new(), None));
    assert!(
        matches!(item.probe(&sys, &ctx), Probe::Blocked { .. }),
        "a stale source cannot read green even if its old inventory is empty"
    );
    assert!(
        item.apply(&sys, &ctx)
            .unwrap_err()
            .contains("profile changed")
    );
    assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
}

#[test]
fn malformed_foreign_duplicate_or_incomplete_pages_never_start_cloning() {
    for variant in 0..5 {
        let (sys, who, config) = fixture();
        let mut values = vec![value("one", 1, true)];
        let mut links = None;
        match variant {
            0 => {
                values[0].as_object_mut().unwrap().remove("default_branch");
            }
            1 => {
                values[0]
                    .as_object_mut()
                    .unwrap()
                    .insert("name".into(), Value::String("../escape".into()));
            }
            2 => {
                values.push(value("two", 1, false));
            }
            3 => {
                links = Some("<https://elsewhere.invalid/repos?page=2>; rel=\"next\"".into());
            }
            4 => {
                values = (0..30)
                    .map(|i| value(&format!("repo{i:03}"), i + 1, false))
                    .collect();
                sys.on_github_api(
                    &who,
                    &format!("{BASE}&page=2"),
                    page(vec![value("omitted", 101, true)], None),
                );
            }
            _ => unreachable!(),
        }
        sys.on_github_api(&who, &format!("{BASE}&page=1"), page(values, links));
        assert!(enumerate(&sys, &who, &config, &token()).is_err());
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("scratch") && !call.contains(" clone "))
        );
    }
    let (sys, who, config) = fixture();
    sys.on_github_api(
        &who,
        &format!("{BASE}&page=1"),
        Err("GitHub's answer was cut short".into()),
    );
    assert!(enumerate(&sys, &who, &config, &token()).is_err());
}

#[test]
fn differing_personal_owner_is_refused_and_organization_scope_is_explicit() {
    let (sys, who, mut config) = fixture();
    config.owner = "other".into();
    sys.on_github_api(
        &who,
        "/users/other",
        json(r#"{"login":"other","type":"User"}"#),
    );
    assert!(
        enumerate(&sys, &who, &config, &token())
            .unwrap_err()
            .contains("organization")
    );
    let (sys, who, mut config) = fixture();
    config.owner = "team".into();
    sys.on_github_api(
        &who,
        "/users/team",
        json(r#"{"login":"team","type":"Organization"}"#),
    );
    sys.on_github_api(
        &who,
        "/orgs/team/repos?type=all&per_page=30&sort=full_name&direction=asc&page=1",
        page(vec![], None),
    );
    assert!(
        enumerate(&sys, &who, &config, &token())
            .unwrap()
            .repositories
            .is_empty()
    );
}

/// `path` a finished checkout of alice's `name`: [`script_worktree`], with a HEAD commit and
/// its index.
pub(in crate::init_github) fn script_existing(sys: &Fake, who: &User, path: &Path, name: &str) {
    script_existing_head(sys, who, path, name, Ran::ok(&format!("{A}\n")));
    script_index(sys, path);
}

/// `path` a top-level worktree of alice's `name` at its GitHub origin ([`script_worktree`])
/// whose HEAD read hears `head`, its index unscripted.
fn script_existing_head(sys: &Fake, who: &User, path: &Path, name: &str, head: Ran) {
    script_worktree(sys, who, path, name);
    script_head(sys, who, path, head);
}

/// What [`verify_existing`] and [`crate::checkout::unborn`] hear of whether `path`'s HEAD
/// names a commit it has ([`crate::checkout::head_commit`]).
pub(in crate::init_github) fn script_head(sys: &Fake, who: &User, path: &Path, reply: Ran) {
    sys.on(
        &crate::checkout::head_commit(sys, who, path).display(),
        reply,
    );
}

/// Where a scripted checkout at `path` keeps its index ([`WORKTREE_READ`]): in a git
/// directory outside the worktree, as a linked worktree's is, so that no staging the fake
/// allocates and no publish rename it checks for a collision meets it.
pub(in crate::init_github) fn fake_index(path: &Path) -> PathBuf {
    Path::new("/fake-git-dirs")
        .join(path.strip_prefix("/").unwrap_or(path))
        .join("index")
}

/// `path`'s index ([`fake_index`]), as a finished checkout leaves it.
pub(in crate::init_github) fn script_index(sys: &Fake, path: &Path) {
    sys.file(fake_index(path).to_str().unwrap(), b"DIRC");
}

/// `path` a top-level worktree of alice's `name` at its GitHub origin, its HEAD unscripted.
pub(in crate::init_github) fn script_worktree(sys: &Fake, who: &User, path: &Path, name: &str) {
    for (args, output) in [
        (
            WORKTREE_READ.to_vec(),
            format!(
                "true\n\n{}\n{}\n",
                path.display(),
                fake_index(path).display()
            ),
        ),
        // What `checkout::read` asks of a worktree it aligns.
        (
            vec!["rev-parse", "--is-inside-work-tree"],
            "true\n".to_owned(),
        ),
        (vec!["rev-parse", "--show-prefix"], String::new()),
        (
            vec!["config", "--get-all", "remote.origin.url"],
            format!("https://github.com/alice/{name}.git\n"),
        ),
        (
            vec!["rev-parse", "--is-shallow-repository"],
            "false\n".into(),
        ),
    ] {
        sys.on(
            &git(sys, who, Some(path)).args(args).display(),
            Ran::ok(&output),
        );
    }
    sys.on(
        &partial_listing(git(sys, who, Some(path))).display(),
        Ran::exit(1, ""),
    );
}

fn script_read(sys: &Fake, who: &User, path: &Path, name: &str, status: &str) {
    for (args, output) in [
        (
            vec!["ls-remote", "--get-url", "origin"],
            format!("https://github.com/alice/{name}.git\n"),
        ),
        (
            vec!["symbolic-ref", "--quiet", "HEAD"],
            "refs/heads/main\n".to_owned(),
        ),
        (
            vec!["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            format!("{A}\n"),
        ),
    ] {
        sys.on(
            &git(sys, who, Some(path)).args(args).display(),
            Ran::ok(&output),
        );
    }
    sys.on(
        &crate::checkout::Programs::listing(git(sys, who, Some(path))).display(),
        Ran::exit(1, ""),
    );
    sys.on(
        &crate::checkout::Programs::default()
            .off(git(sys, who, Some(path)))
            .args(crate::checkout::NO_SIDE_RUNS)
            .args([
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
                "--ignore-submodules=all",
            ])
            .display(),
        Ran::ok(status),
    );
}

#[test]
fn a_clean_worktree_behind_github_is_fast_forwarded_and_a_recorded_one_left_to_develop() {
    use std::sync::{Arc, Mutex};
    for behind in [true, false] {
        let (sys, who, config) = fixture();
        let home = Path::new(&who.home);
        let (moving, dirty, recorded) =
            (home.join("moving"), home.join("dirty"), home.join("aterm"));
        for (path, name) in [(&moving, "moving"), (&dirty, "dirty"), (&recorded, "aterm")] {
            sys.stat_of(path.to_str().unwrap(), directory(who.uid));
            script_existing(&sys, &who, path, name);
        }
        script_read(&sys, &who, &dirty, "dirty", " M README\0");
        sys.on(
            &crate::checkout::network(&sys, &who, Some(&dirty))
                .args(["ls-remote", "--symref", "origin", "HEAD"])
                .display(),
            Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{B}\tHEAD\n")),
        );
        script_read(&sys, &who, &moving, "moving", "");
        let state = Arc::new(Mutex::new((if behind { A } else { B }.to_owned(), false)));
        let model = {
            let (state, moving) = (Arc::clone(&state), moving.clone());
            move |_: &Fake, cmd: &Cmd| -> Option<Ran> {
                let args: Vec<String> = cmd
                    .args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                if args.get(1).map(PathBuf::from) != Some(moving.clone()) {
                    return None;
                }
                let mut state = state.lock().unwrap();
                let (head, fetched) = (state.0.clone(), state.1);
                let mut operation = &args[2..];
                while operation.first().map(String::as_str) == Some("-c")
                    && operation
                        .get(1)
                        .is_some_and(|value| value.starts_with("credential."))
                {
                    operation = &operation[2..];
                }
                let line = operation.join(" ");
                Some(match line.as_str() {
                    "rev-parse --verify --quiet HEAD^{commit}" => Ran::ok(&format!("{head}\n")),
                    "ls-remote --symref origin HEAD" => {
                        Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{B}\tHEAD\n"))
                    }
                    line if line == format!("rev-parse --verify --quiet {B}^{{commit}}") => {
                        if fetched {
                            Ran::ok(&format!("{B}\n"))
                        } else {
                            Ran::exit(1, "")
                        }
                    }
                    "rev-list --count HEAD --not --remotes=origin" => Ran::ok("0\n"),
                    "-c core.hooksPath=/dev/null -c core.fsmonitor= -c maintenance.auto=false fetch --progress --no-recurse-submodules origin" =>
                    {
                        state.1 = true;
                        Ran::ok("")
                    }
                    "rev-parse --verify --quiet refs/remotes/origin/main^{commit}" => {
                        Ran::ok(&format!("{B}\n"))
                    }
                    line if line == format!("merge-base --is-ancestor {A} {B}") => Ran::ok(""),
                    line if line == format!("rev-list --count {A}..{B}") => Ran::ok("1\n"),
                    line if line
                        == format!(
                            "-c core.hooksPath=/dev/null -c core.fsmonitor= -c maintenance.auto=false -c submodule.recurse=false merge --ff-only --no-autostash --quiet {B}"
                        ) =>
                    {
                        state.0 = B.to_owned();
                        Ran::ok("")
                    }
                    _ => return None,
                })
            }
        };
        sys.simulate(model);
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(
                vec![
                    value("aterm", 1, true),
                    value("dirty", 2, true),
                    value("moving", 3, true),
                ],
                None,
            ),
        );
        let probe = item(&who, &config, vec![recorded.clone()])
            .probe(&sys, &ctx(std::slice::from_ref(&who)));
        let rows = engine::run(
            &[item(&who, &config, vec![recorded.clone()])],
            Mode::Apply,
            &sys,
            &ctx(std::slice::from_ref(&who)),
            &mut NoJournal,
        );
        let calls = sys.calls();
        if behind {
            assert!(
                matches!(&probe, Probe::Change { from, .. } if from.contains("1 clean worktree(s) behind GitHub")),
                "{probe:?}"
            );
            assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
            assert_eq!(state.lock().unwrap().0, B);
            assert_eq!(
                calls.iter().filter(|call| call.contains(" fetch ")).count(),
                1
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.contains(" merge --ff-only "))
                    .count(),
                1
            );
        } else {
            assert!(matches!(&probe, Probe::Ok { .. }), "{probe:?}");
            assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
            assert!(
                !calls
                    .iter()
                    .any(|call| call.contains(" fetch ") || call.contains(" merge "))
            );
        }
        let after = item(&who, &config, vec![recorded.clone()])
            .probe(&sys, &ctx(std::slice::from_ref(&who)));
        assert!(
            matches!(&after, Probe::Ok { now } if now.contains("1 recorded in [develop]")
                && now.contains("~/dirty: it has uncommitted changes")),
            "{after:?}"
        );
        let head_read = git(&sys, &who, Some(&recorded))
            .args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .display();
        let recorded_reads: Vec<&String> = calls
            .iter()
            .filter(|call| call.contains(&format!("-C {} ", recorded.display())))
            .collect();
        assert!(
            recorded_reads
                .iter()
                .all(|call| call.contains(" rev-parse --is-inside-work-tree")
                    || call.contains(" rev-parse --show-prefix")
                    || call.contains(" rev-parse --show-toplevel")
                    || call.contains(" config --get-all remote.origin.url")
                    || **call == head_read),
            "{recorded_reads:?}"
        );
        assert!(
            recorded_reads.contains(&&head_read),
            "the delegated worktree still needs a proven HEAD"
        );
        for word in [
            "reset", "stash", "rebase", "checkout", "switch", "pull", "push",
        ] {
            assert!(
                !calls.iter().any(|call| call.contains(&format!(" {word} "))
                    || call.ends_with(&format!(" {word}"))),
                "{word}: {calls:?}"
            );
        }
    }
}

const GIB: u64 = aterm_capacity::floor::GIB;

/// A data volume of 100 GiB with `free` bytes free and a floor of 8 GiB.
fn volume(free: u64) -> aterm_capacity::reading::Volume {
    aterm_capacity::reading::Volume {
        mount: PathBuf::from("/System/Volumes/Data"),
        total: 100 * GIB,
        free,
        floor: 8 * GIB,
        room: 8 * GIB,
    }
}

fn sized(name: &str, id: u64, size_kib: u64) -> Value {
    let mut value = value(name, id, true);
    value
        .as_object_mut()
        .unwrap()
        .insert("size".into(), aterm_json::json!(size_kib));
    value
}

/// A clone runs while five times GitHub's size for it leaves the disk watch's floor free, to
/// the byte; one that would not is named and left; an unread volume runs every clone.
#[test]
fn a_clone_runs_only_while_it_leaves_the_disk_watchs_floor_free() {
    let repo = |size_kib| Repo {
        owner: "alice".into(),
        name: "big".into(),
        default_branch: Some("main".into()),
        size_kib,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    };
    assert_eq!(no_room(None, &repo(u64::MAX)), None);
    assert_eq!(short_of_room(None, &[], INIT_AGAIN), None);

    // GitHub's 0 (empty, or not sized yet) still needs room above the floor.
    assert_eq!(no_room(Some(&volume(8 * GIB + 5 * 1024)), &repo(0)), None);
    assert_eq!(
        no_room(Some(&volume(8 * GIB + 5 * 1024 - 1)), &repo(0)).as_deref(),
        Some("~/big")
    );
    assert_eq!(
        no_room(Some(&volume(3 * GIB)), &repo(0)).as_deref(),
        Some("~/big")
    );

    let volume = volume(8 * GIB + 5 * 1000 * 1024);
    assert_eq!(no_room(Some(&volume), &repo(1000)), None);
    let left = [
        no_room(Some(&volume), &repo(1001)).unwrap(),
        no_room(Some(&volume), &repo(2 * 1024 * 1024)).unwrap(),
    ];
    assert_eq!(left, ["~/big", "~/big (about 10 GiB)"]);
    assert_eq!(short_of_room(Some(&volume), &[], INIT_AGAIN), None);
    assert_eq!(
        short_of_room(Some(&volume), &left, "try again").unwrap(),
        "2 repositories not cloned, for want of disk space: ~/big, ~/big (about 10 GiB); \
         /System/Volumes/Data has 8 GiB free and keeps 8 GiB free; free space, then try again"
    );
}

/// The plan says how many missing repositories the disk has no room for; a pass leaves
/// out each of them, still clones the next one that fits, and ends naming the one it left.
#[test]
fn a_repository_the_disk_has_no_room_for_is_left_and_the_rest_cloned() {
    let (mut sys, who, config) = fixture();
    sys.volume = Some(volume(12 * GIB));
    let (_, _, clone_key) = script_clone(&sys, &who, false);
    sys.on_github_api(
        &who,
        &format!("{BASE}&page=1"),
        page(
            vec![sized("huge", 1, 2 * 1024 * 1024), value("new", 2, true)],
            None,
        ),
    );
    let ctx = ctx(std::slice::from_ref(&who));
    let repositories = item(&who, &config, Vec::new());
    let probe = repositories.probe(&sys, &ctx);
    assert!(matches!(probe, Probe::Change { .. }), "{probe:?}");
    assert_eq!(
        repositories.detail(&sys, &ctx),
        Some(
            Reading::Repositories {
                present: 0,
                count: 2,
                signed_in: true,
            }
            .token()
        )
    );
    let why = item(&who, &config, Vec::new())
        .apply(&sys, &ctx)
        .unwrap_err();
    assert!(!why.is_empty(), "an unfinished clone needs a diagnostic");
    let calls = sys.calls();
    assert_eq!(calls.iter().filter(|call| **call == clone_key).count(), 1);
    assert!(
        calls.iter().all(|call| !call.contains("alice/huge")),
        "{calls:?}"
    );
}

/// A pass reads the disk once, and each clone it admits holds its room for the ones after
/// it: of two repositories that each fit alone but not together, the first clones and the
/// second is left with the reason, never started.
#[test]
fn each_clone_a_pass_admits_holds_its_room_for_the_ones_after_it() {
    let (mut sys, who, _config) = fixture();
    // 8 GiB above the floor: room for one clone of about 5 GiB, not for two.
    sys.volume = Some(volume(16 * GIB));
    let sized = |name: &str, sequence| {
        let (mut repo, _, _) = script_clone_named(&sys, &who, name, sequence, false);
        repo.size_kib = GIB / 1024;
        repo
    };
    let missing = [sized("one", 0), sized("two", 1)];
    let cloned = clone_all(&sys, &who, &missing, None);
    let names: Vec<&str> = cloned
        .cloned
        .iter()
        .map(|repo| repo.name.as_str())
        .collect();
    assert_eq!(names, ["one"]);
    assert!(cloned.failed.is_empty(), "{:?}", cloned.failed);
    assert_eq!(cloned.left, ["~/two (about 5 GiB)"]);
    let observed = sys.repository_observations();
    let one = observed
        .iter()
        .filter(|p| p.name == "one")
        .collect::<Vec<_>>();
    assert_eq!(one.first().unwrap().phase, Phase::Queued);
    assert_eq!(one.last().unwrap().phase, Phase::Ready);
    assert!(one.last().unwrap().usable);
    let placing = one.iter().position(|p| p.phase == Phase::Placing).unwrap();
    let dependencies = one
        .iter()
        .position(|p| p.phase == Phase::Dependencies)
        .unwrap();
    assert!(placing < dependencies);
    assert!(one[..=placing].iter().all(|p| !p.usable));
    assert!(
        observed
            .iter()
            .any(|p| p.name == "two" && p.phase == Phase::WaitingForSpace)
    );
    let calls = sys.calls();
    assert!(
        calls.iter().all(|call| !call.contains("alice/two")),
        "{calls:?}"
    );
}

#[test]
fn a_worktree_whose_origin_cannot_be_read_never_stops_a_clone() {
    for (stderr, offline) in [
        (
            "git@github.com: Permission denied (publickey).\nfatal: Could not read from \
             remote repository.\n",
            false,
        ),
        (
            "fatal: unable to access 'https://github.com/alice/present.git/': Could not \
             resolve host: github.com\n",
            true,
        ),
    ] {
        let (sys, who, config) = fixture();
        let present = Path::new(&who.home).join("present");
        sys.stat_of(present.to_str().unwrap(), directory(who.uid));
        script_existing(&sys, &who, &present, "present");
        script_read(&sys, &who, &present, "present", "");
        sys.on(
            &crate::checkout::network(&sys, &who, Some(&present))
                .args(["ls-remote", "--symref", "origin", "HEAD"])
                .display(),
            Ran::exit(128, stderr),
        );
        let (_, _, clone_key) = script_clone(&sys, &who, false);
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(vec![value("new", 1, true), value("present", 2, true)], None),
        );
        let origin = Origin(vec![crate::checkout::Remote::Github {
            owner: "alice".into(),
            name: "present".into(),
        }]);
        let unread = crate::checkout::read(&sys, &who, &present, &origin).unwrap_err();
        assert_eq!(unread.offline, offline);
        let ctx = ctx(std::slice::from_ref(&who));
        let item = item(&who, &config, Vec::new());
        match item.probe(&sys, &ctx) {
            Probe::Change { from, .. } => {
                assert!(from.starts_with("1 of 2 repositories missing"), "{from}");
            }
            other => panic!("{other:?}"),
        }
        let scan = Repositories {
            user: who.clone(),
            config: config.clone(),
            recorded: Vec::new(),
            empty: std::sync::Mutex::new(None),
            lease: std::sync::Mutex::new(None),
            hooks: std::sync::Mutex::new(Vec::new()),
            reading: LastReading::default(),
        }
        .scan(&sys, &token(), &BTreeSet::new())
        .unwrap();
        assert!(
            scan.kept()
                .is_some_and(|kept| kept.starts_with("~/present: ") && kept.contains(&unread.why)),
            "{:?}",
            scan.kept()
        );
        item.apply(&sys, &ctx).unwrap();
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| **call == clone_key)
                .count(),
            1
        );
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(" fetch ") && !call.contains(" merge ")),
            "{:?}",
            sys.calls()
        );
    }
}

#[test]
fn the_profile_s_develop_checkouts_are_left_to_the_develop_items() {
    for develop in [true, false] {
        let (sys, who, _) = fixture();
        let aterm = Path::new(&who.home).join("aterm");
        sys.stat_of(aterm.to_str().unwrap(), directory(who.uid));
        script_existing(&sys, &who, &aterm, "aterm");
        script_read(&sys, &who, &aterm, "aterm", "");
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(vec![value("aterm", 1, true)], None),
        );
        let mut profile = "[github]\naccount = \"alice\"\nowner = \"alice\"\ntoken_file = \
                           \"~/.secrets/github\"\n"
            .to_owned();
        if develop {
            profile.push_str("\n[develop]\ncheckouts = [\"~/aterm\"]\n");
        }
        sys.write_user_file(
            &who,
            &Path::new(&who.home).join(".config/aterm/init.toml"),
            profile.as_bytes(),
            0o600,
        )
        .unwrap();
        // After the profile: its `token_file` is what the route's helper names.
        let remote_head = crate::checkout::network(&sys, &who, Some(&aterm))
            .args(["ls-remote", "--symref", "origin", "HEAD"])
            .display();
        sys.on(
            &remote_head,
            Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{A}\tHEAD\n")),
        );
        let ctx = ctx(std::slice::from_ref(&who));
        let items = crate::init_github::items(&sys, &ctx, None).unwrap();
        let repositories = items
            .iter()
            .find(|item| item.id().starts_with("init.github.repositories@"))
            .unwrap();
        let probe = repositories.probe(&sys, &ctx);
        let read = sys.calls().contains(&remote_head);
        if develop {
            assert!(
                matches!(&probe, Probe::Ok { now } if now.contains("1 recorded in [develop]")),
                "{probe:?}"
            );
            assert!(!read, "{:?}", sys.calls());
        } else {
            assert!(
                matches!(&probe, Probe::Ok { now } if !now.contains("[develop]")),
                "{probe:?}"
            );
            assert!(read, "{:?}", sys.calls());
        }
    }
}

#[test]
fn plan_and_check_probe_do_not_clone_or_change_existing_branches_or_files() {
    let (sys, who, config) = fixture();
    let home = Path::new(&who.home);
    let existing = home.join("existing");
    sys.stat_of(existing.to_str().unwrap(), directory(who.uid));
    sys.file(
        existing.join("dirty.txt").to_str().unwrap(),
        b"owner changes",
    );
    script_existing(&sys, &who, &existing, "existing");
    script_read(&sys, &who, &existing, "existing", "?? dirty.txt\0");
    sys.on_github_api(
        &who,
        &format!("{BASE}&page=1"),
        page(
            vec![value("existing", 1, true), value("missing", 2, true)],
            None,
        ),
    );
    let rows = engine::run(
        &[item(&who, &config, Vec::new())],
        Mode::Plan,
        &sys,
        &ctx(std::slice::from_ref(&who)),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Planned);
    assert_eq!(
        sys.read_file(&existing.join("dirty.txt")).unwrap(),
        b"owner changes"
    );
    assert!(sys.calls().iter().all(|call| !call.contains("scratch")
        && !call.contains(" clone ")
        && !call.contains("rename")
        && !call.contains("checkout")
        && !call.contains("fetch")
        && !call.contains("pull")
        && !call.contains("reset")));
}

/// Repositories read on several lanes still stand in the order GitHub listed them.
#[test]
fn a_partition_read_on_lanes_keeps_the_list_order() {
    let (sys, who, _) = fixture();
    let mut names: Vec<String> = (0..3 * READ_LANES)
        .map(|index| format!("r{index:02}"))
        .collect();
    names.insert(READ_LANES, "Music".to_owned());
    let mut missing = Vec::new();
    let mut notes = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let path = Path::new(&who.home).join(name);
        if name == "Music" {
            notes.push("~/Music is a standard home folder; left untouched".to_owned());
            continue;
        }
        match index % 3 {
            0 => {
                missing.push(name.clone());
                continue;
            }
            1 => sys.file(path.to_str().unwrap(), b"owner file"),
            _ => sys.link(path.to_str().unwrap(), "/elsewhere"),
        };
        notes.push(format!("~/{name} is occupied or linked; left untouched"));
    }
    let all: Vec<Repo> = names
        .iter()
        .zip(1..)
        .map(|(name, id)| repo(&value(name, id, true), Some("alice")).unwrap())
        .collect();
    let standing = partition(&sys, &who, &all).unwrap();
    let read: Vec<_> = standing.missing.iter().map(|repo| &repo.name).collect();
    assert_eq!(read, missing.iter().collect::<Vec<_>>());
    assert_eq!(standing.notes, notes);
    assert!(standing.present.is_empty() && standing.unborn.is_empty());
}

/// A file, a link or another origin's worktree where a selected repository goes is left as
/// it is. The repository remains in the desired count and the stage stays unfinished.
#[test]
fn occupied_symlink_and_mismatched_origins_are_left_as_notes_and_never_fetched() {
    for kind in 0..3 {
        let (sys, who, config) = fixture();
        let path = Path::new(&who.home).join("one");
        match kind {
            0 => {
                sys.file(path.to_str().unwrap(), b"owner file");
            }
            1 => {
                sys.link(path.to_str().unwrap(), "/elsewhere");
            }
            2 => {
                sys.stat_of(path.to_str().unwrap(), directory(who.uid));
                let cmd =
                    git(&sys, &who, Some(&path)).args(["config", "--get-all", "remote.origin.url"]);
                sys.on(
                    &cmd.display(),
                    Ran::ok("https://private-token@github.com/alice/one.git\n"),
                );
                script_existing(&sys, &who, &path, "one");
            }
            _ => unreachable!(),
        }
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(vec![value("one", 1, true)], None),
        );
        let row = engine::run(
            &[item(&who, &config, Vec::new())],
            Mode::Apply,
            &sys,
            &ctx(std::slice::from_ref(&who)),
            &mut NoJournal,
        )
        .remove(0);
        assert_eq!(row.outcome, Outcome::Person, "{row:?}");
        assert_eq!(
            crate::init_observe::Reading::of(&row),
            Some(crate::init_observe::Reading::Repositories {
                present: 0,
                count: 1,
                signed_in: true,
            }),
        );
        let why = &row
            .explain
            .as_ref()
            .expect("the preserved destination is explained")
            .why;
        assert!(why.contains("~/one"), "{why}");
        assert!(!why.contains("private-token"), "{why}");
        assert!(sys.calls().iter().all(|call| !call.contains("scratch")
            && !call.contains(" clone ")
            && !call.contains("rename")
            && !call.contains("fetch")));
        assert!(sys.stat(&path).is_ok());
    }
}

fn script_clone(sys: &Fake, who: &User, empty: bool) -> (Repo, PathBuf, String) {
    script_clone_named(sys, who, "new", 0, empty)
}

#[test]
fn deferred_alignment_records_the_local_condition_and_keeps_the_checkout_usable() {
    use crate::repository_progress::{OutcomeReason, PreservationReason, RetryDisposition};
    let (sys, who, _) = fixture();
    let path = Path::new(&who.home).join("dirty");
    script_existing(&sys, &who, &path, "dirty");
    script_read(&sys, &who, &path, "dirty", "?? owner-work\0");
    let origin = crate::checkout::Origin(vec![crate::checkout::Remote::Github {
        owner: "alice".into(),
        name: "dirty".into(),
    }]);
    assert!(matches!(
        crate::checkout::align(&sys, &who, &path, &origin),
        Ok(Stands::Kept(_))
    ));
    let rows = sys.repository_observations();
    let row = rows.last().unwrap();
    assert_eq!(row.phase, Phase::Deferred);
    assert!(row.usable && row.placed);
    let outcome = row.outcome.as_ref().unwrap();
    assert_eq!(outcome.stage, Phase::Checking);
    assert_eq!(
        outcome.reason,
        OutcomeReason::Preserved(PreservationReason::LocalChanges)
    );
    assert_eq!(outcome.retry, RetryDisposition::AfterChanges);
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains(" fetch ") && !call.contains(" merge "))
    );
}

#[test]
fn clone_failures_publish_typed_reasons_without_transport_or_callback_diagnostics() {
    use crate::repository_progress::{FailureKind, OutcomeReason, RetryDisposition};
    let diagnostic = [
        "https://",
        "fixture-user:",
        "fixture-value",
        "@example.invalid/repo",
    ]
    .concat();
    for network in [false, true] {
        let (sys, who, _) = fixture();
        let (repo, _, _) = script_clone(&sys, &who, false);
        let stderr = diagnostic.clone();
        sys.simulate(move |_, command| {
            (network && command.args.iter().any(|arg| arg == "clone"))
                .then(|| Ran::exit(128, &stderr))
        });
        let current = || {
            if network {
                Ok(())
            } else {
                Err(diagnostic.clone())
            }
        };
        let guard = CloneGuard::new(&current);
        let result = clone_all(&sys, &who, &[repo], Some(&guard));
        assert_eq!(result.failed.len(), 1);
        let observed = sys.repository_observations();
        let row = observed.last().unwrap();
        assert_eq!(row.phase, Phase::Failed);
        assert!(!row.usable && !row.placed);
        let outcome = row.outcome.as_ref().unwrap();
        assert_eq!(
            outcome.stage,
            if network {
                Phase::Connecting
            } else {
                Phase::Queued
            }
        );
        assert_eq!(
            outcome.reason,
            OutcomeReason::Failed(if network {
                FailureKind::Network
            } else {
                FailureKind::Local
            })
        );
        assert_eq!(outcome.retry, RetryDisposition::Unrecorded);
        let snapshot = aterm_json::to_string(row).unwrap();
        assert!(!snapshot.contains("fixture-value") && !snapshot.contains("://"));
        assert!(!outcome.reason.label().contains("authentication"));
    }
}

/// A clone of github.com/alice/`name` into the `sequence`th staging directory the run
/// allocates, scripted from its first read to its last.
pub(in crate::init_github) fn script_clone_named(
    sys: &Fake,
    who: &User,
    name: &str,
    sequence: u64,
    empty: bool,
) -> (Repo, PathBuf, String) {
    script_clone_named_route(sys, who, name, sequence, empty, false)
}

/// Where the `sequence`th clone of github.com/alice/`name` stages its repository, and the
/// display of that clone's command, under which a script adds what the clone writes.
pub(in crate::init_github) fn clone_key(
    sys: &Fake,
    who: &User,
    name: &str,
    sequence: u64,
) -> (PathBuf, String) {
    let staged =
        Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", sequence).join("repo");
    let command = crate::checkout::network(sys, who, None)
        .budget(CLONE_BUDGET)
        .args([
            "clone",
            "--no-single-branch",
            "--tags",
            "--origin",
            "origin",
            "--template=",
            "--progress",
            "--",
            &format!("https://github.com/alice/{name}.git"),
        ])
        .arg(&staged);
    (staged, command.display())
}

/// [`script_clone_named`], its effective URL a same-repository SSH rewrite when `rewritten`:
/// the person's own URL, which the clone keeps, under aterm's credential route.
pub(in crate::init_github) fn script_clone_named_route(
    sys: &Fake,
    who: &User,
    name: &str,
    sequence: u64,
    empty: bool,
    rewritten: bool,
) -> (Repo, PathBuf, String) {
    let repo = Repo {
        owner: "alice".into(),
        name: name.into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    };
    let url = format!("https://github.com/alice/{name}.git");
    let url = url.as_str();
    let endpoint = if rewritten {
        format!("git@github.com:alice/{name}.git")
    } else {
        url.to_owned()
    };
    sys.on(
        &git(sys, who, None)
            .args(["ls-remote", "--get-url", "--", url])
            .display(),
        Ran::ok(&endpoint),
    );
    let route = || crate::checkout::network(sys, who, None);
    let remote = if empty {
        String::new()
    } else {
        format!(
            "ref: refs/heads/main\tHEAD\n{A}\tHEAD\n{A}\trefs/heads/main\n{B}\trefs/heads/topic\n{B}\trefs/tags/v1\n"
        )
    };
    let command = route().args([
        "ls-remote",
        "--symref",
        "--",
        url,
        "HEAD",
        "refs/heads/*",
        "refs/tags/*",
    ]);
    sys.on(&command.display(), Ran::ok(&remote));
    let (staged, clone_key) = clone_key(sys, who, name, sequence);
    sys.on(&clone_key, Ran::ok(""));
    sys.on_stat(&clone_key, &staged, directory(who.uid));
    sys.on_stat(&clone_key, &staged.join(".git"), directory(who.uid));
    sys.on_stat(&clone_key, &staged.join(".git/objects"), directory(who.uid));
    sys.on_write(&clone_key, &staged.join(".git/fixture"), b"complete clone");
    script_existing_head(
        sys,
        who,
        &staged,
        name,
        if empty {
            Ran::exit(1, "")
        } else {
            Ran::ok(&format!("{A}\n"))
        },
    );
    if !empty {
        // A finished clone of a commit: git wrote its index as the clone ran.
        sys.on_write(&clone_key, &fake_index(&staged), b"DIRC");
    }
    if empty {
        let destination = Path::new(&who.home).join(name);
        script_existing_head(sys, who, &destination, name, Ran::exit(1, ""));
        for (index, path) in [
            destination.clone(),
            destination.join(".git"),
            destination.join(".git/objects"),
        ]
        .into_iter()
        .enumerate()
        {
            sys.file_id_of(
                &path,
                FileId {
                    dev: 1,
                    ino: 100 + sequence * 3 + index as u64,
                    born_ns: Some(1),
                },
            );
        }
        for (args, ran) in [
            (
                vec!["symbolic-ref", "--quiet", "--short", "HEAD"],
                Ran::ok("main\n"),
            ),
            (
                vec!["show-ref", "--verify", "--quiet", "refs/heads/main"],
                Ran::exit(1, ""),
            ),
            (
                vec!["rev-parse", "--git-path", "objects/info/alternates"],
                Ran::ok(".git/objects/info/alternates\n"),
            ),
            (
                vec!["for-each-ref", "--format=%(objectname) %(refname)"],
                Ran::ok(""),
            ),
            (
                vec!["ls-remote", "--get-url", "--", "origin"],
                Ran::ok(&endpoint),
            ),
        ] {
            sys.on(&git(sys, who, Some(&destination)).args(args).display(), ran);
        }
    }
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["rev-parse", "--git-path", "objects/info/alternates"])
            .display(),
        Ran::ok(".git/objects/info/alternates\n"),
    );
    let head = git(sys, who, Some(&staged)).args(["rev-parse", "--verify", "HEAD^{commit}"]);
    sys.on(
        &head.display(),
        if empty {
            Ran::exit(128, "unborn")
        } else {
            Ran::ok(A)
        },
    );
    let refs = if empty {
        String::new()
    } else {
        format!(
            "{A} refs/heads/main\n{A} refs/remotes/origin/HEAD\n{A} refs/remotes/origin/main\n{B} refs/remotes/origin/topic\n{B} refs/tags/v1\n"
        )
    };
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["for-each-ref", "--format=%(objectname) %(refname)"])
            .display(),
        Ran::ok(&refs),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .display(),
        Ran::ok(""),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .display(),
        Ran::ok("refs/heads/main\n"),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
            .display(),
        Ran::ok("refs/remotes/origin/main\n"),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["config", "--get", "branch.main.remote"])
            .display(),
        Ran::ok("origin\n"),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["config", "--get", "branch.main.merge"])
            .display(),
        Ran::ok("refs/heads/main\n"),
    );
    sys.on(
        &git(sys, who, Some(&staged))
            .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
            .display(),
        Ran::ok(""),
    );
    (repo, staged, clone_key)
}

/// A clone's publish rename is not repeated safely, so it holds the update boundary while
/// it runs (init-on-by-default design §6.7): an update's handoff waits for it, boundedly.
/// Before, nothing held the guard, and a handoff could cut the publish in half.
#[test]
fn a_clones_publish_rename_holds_the_update_boundary() {
    let (sys, who, _) = fixture();
    let (repo, _, _) = script_clone(&sys, &who, false);
    clone_one(&sys, &who, "alice", &repo).unwrap();
    assert_eq!(
        sys.guarded_renames(),
        [Path::new(&who.home).join("new")],
        "the publish rename ran under the guard"
    );
}

/// A guarded clone checks `current` before every step, the publish included. The publish's
/// guard waits for a pending update handoff (design §6.7), up to minutes, after the last
/// check; once the handoff is refused it checks `current` again, and publishes nothing when
/// the vault changed meanwhile. Before, the clone was published on the check made before the
/// wait.
#[test]
fn a_clone_publish_that_waited_for_a_handoff_checks_current_again() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for changes in [true, false] {
        let (sys, who, _) = fixture();
        let changed = AtomicBool::new(false);
        let current = || {
            if changed.load(Ordering::SeqCst) {
                Err("injected: the vault changed while the publish waited".to_owned())
            } else {
                Ok(())
            }
        };
        let (repo, _staged, _) = script_clone(&sys, &who, false);
        let latch = sys.boundary.latch_handoff(Duration::from_secs(60));
        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(|| clone_one_guarded(&sys, &who, &repo, &current));
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while sys.boundary.waiting() == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the publish never waited"
                );
                aterm_uds::deadline::poll_yield();
            }
            changed.store(changes, Ordering::SeqCst);
            drop(latch);
            worker.join().unwrap()
        });
        let published = sys
            .calls()
            .iter()
            .any(|call| call.starts_with("rename-noreplace "));
        if changes {
            assert!(
                !published,
                "the clone was published after the vault changed during its wait: {result:?}"
            );
            assert!(
                matches!(&result, Err(why) if why.contains("the vault changed")),
                "{result:?}"
            );
        } else {
            // CONTROL: nothing changed while it waited, and it publishes once.
            assert!(published && result.is_ok(), "{result:?}");
        }
        assert_eq!(sys.boundary.any_held(), None);
    }
}

#[test]
fn full_and_empty_new_clones_are_verified_then_published_without_credentials_in_origin() {
    for empty in [false, true] {
        let (sys, who, config) = fixture();
        let (repo, staged, _) = script_clone(&sys, &who, empty);
        clone_one(&sys, &who, "alice", &repo).unwrap();
        let final_path = Path::new(&who.home).join("new");
        assert_eq!(
            sys.read_file(&final_path.join(".git/fixture")).unwrap(),
            b"complete clone"
        );
        assert!(sys.stat(&staged).is_err());
        let calls = sys.calls();
        let integrity = calls
            .iter()
            .position(|call| call.contains("rev-parse --is-shallow-repository"))
            .unwrap();
        let published = calls
            .iter()
            .position(|call| call.starts_with("rename-noreplace "))
            .unwrap();
        assert!(integrity < published);
        // The clone checked its objects' connectivity itself: nothing reads them again.
        assert!(
            calls.iter().all(|call| !call.contains(" fsck ")),
            "{calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|call| call.contains(" clone ")).count(),
            1
        );
        if empty {
            // The real Profile item must observe emptiness, then reuse only that
            // item's fact for apply/readback; owner work and [develop] stay theirs.
            let endpoint = "https://github.com/alice/new.git";
            for args in [
                vec!["ls-remote", "--get-url", "--", endpoint],
                vec!["ls-remote", "--get-url", "origin"],
            ] {
                sys.on(
                    &git(&sys, &who, Some(&final_path)).args(args).display(),
                    Ran::ok(endpoint),
                );
            }
            sys.on(
                &git(&sys, &who, Some(&final_path))
                    .args(["symbolic-ref", "--quiet", "HEAD"])
                    .display(),
                Ran::ok("refs/heads/main\n"),
            );
            let advertisement = crate::checkout::network(&sys, &who, Some(&final_path))
                .args([
                    "ls-remote",
                    "--symref",
                    "--",
                    endpoint,
                    "HEAD",
                    "refs/heads/*",
                    "refs/tags/*",
                ])
                .display();
            sys.on(&advertisement, Ran::ok(""));
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(vec![value("new", 1, true)], None),
            );
            let owned = final_path.join("owner-notes");
            sys.file(owned.to_str().unwrap(), b"the person's work");
            let context = ctx(std::slice::from_ref(&who));
            for recorded in [false, true] {
                let block = item(
                    &who,
                    &config,
                    if recorded {
                        vec![final_path.clone()]
                    } else {
                        Vec::new()
                    },
                );
                let before = sys
                    .calls()
                    .iter()
                    .filter(|call| **call == advertisement)
                    .count();
                assert!(matches!(block.probe(&sys, &context), Probe::Ok { .. }));
                block.apply(&sys, &context).unwrap();
                assert!(matches!(block.probe(&sys, &context), Probe::Ok { .. }));
                assert_eq!(
                    sys.calls()
                        .iter()
                        .filter(|call| **call == advertisement)
                        .count(),
                    before + 1
                );
                assert_eq!(
                    block.detail(&sys, &context),
                    Some(
                        Reading::Repositories {
                            present: 1,
                            count: 1,
                            signed_in: true
                        }
                        .token()
                    )
                );
                assert_eq!(sys.read_file(&owned).unwrap(), b"the person's work");
                assert!(
                    sys.calls()
                        .iter()
                        .all(|call| !call.contains(" fetch ") && !call.contains(" checkout "))
                );
            }
            let profile = format!(
                "[github]\naccount='alice'\nowner='alice'\ntoken_file='{SOURCE}'\n[develop]\ncheckouts=['~/new']\n"
            );
            sys.write_user_file(
                &who,
                &Path::new(&who.home).join(".config/aterm/init.toml"),
                profile.as_bytes(),
                0o600,
            )
            .unwrap();
            let newly_delegated = item(&who, &config, Vec::new());
            assert!(
                matches!(newly_delegated.probe(&sys, &context), Probe::Ok { now } if now.contains("1 recorded in [develop]"))
            );
            assert_eq!(
                newly_delegated.detail(&sys, &context),
                Some(
                    Reading::Repositories {
                        present: 1,
                        count: 1,
                        signed_in: true
                    }
                    .token()
                )
            );
            let changed = advertisement.clone();
            sys.simulate(move |_, command| {
                (command.display() == changed).then(|| {
                    Ran::ok(&format!(
                        "ref: refs/heads/main\tHEAD\n{A}\tHEAD\n{A}\trefs/heads/main\n"
                    ))
                })
            });
            let delegated = item(&who, &config, vec![final_path.clone()]);
            assert!(
                matches!(delegated.probe(&sys, &context), Probe::Person { how, .. } if how.contains("[develop]"))
            );
            assert_eq!(
                delegated.detail(&sys, &context),
                Some(
                    Reading::Repositories {
                        present: 0,
                        count: 1,
                        signed_in: true
                    }
                    .token()
                )
            );
            assert_eq!(sys.read_file(&owned).unwrap(), b"the person's work");
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains(" fetch ") && !call.contains(" checkout "))
            );
        }
    }
    for endpoint in [
        "git@github.com:alice/new.git",
        "ssh://git@github.com:22/alice/new.git",
    ] {
        let (sys, who, _) = fixture();
        let key = git(&sys, &who, None)
            .args([
                "ls-remote",
                "--get-url",
                "--",
                "https://github.com/alice/new.git",
            ])
            .display();
        sys.on(&key, Ran::ok(endpoint)).on(&key, Ran::ok(endpoint));
        let (repo, _, _) = script_clone(&sys, &who, false);
        clone_one(&sys, &who, "alice", &repo).unwrap();
        assert!(sys.stat(&Path::new(&who.home).join("new")).unwrap().dir);
    }
}

/// A guarded clone carries aterm's credential route and no keychain helper, and refuses to
/// go on once the vault snapshot changed.
#[test]
fn guarded_clones_carry_the_route_and_refuse_newer_versions() {
    for changed in [false, true] {
        let (sys, who, _) = fixture();
        let current = || {
            if changed && sys.calls().iter().any(|call| call.contains(" clone ")) {
                Err("injected newer vault version".into())
            } else {
                Ok(())
            }
        };
        let (repo, _staged, _) = script_clone(&sys, &who, false);
        let result = clone_one_guarded(&sys, &who, &repo, &current);
        assert_eq!(result.is_ok(), !changed, "{result:?}");
        let final_path = Path::new(&who.home).join("new");
        assert_eq!(sys.stat(&final_path).is_ok(), !changed);
        let calls = sys.calls();
        let network: Vec<_> = calls
            .iter()
            .filter(|call| call.contains("ls-remote --symref") || call.contains(" clone "))
            .collect();
        assert_eq!(network.len(), 2, "{calls:?}");
        assert!(
            network.iter().all(
                |call| call.contains("-c credential.helper= ") && !call.contains("osxkeychain")
            ),
            "{network:?}"
        );
        assert!(calls.iter().all(|call| !call.contains("--global")));
        assert_eq!(
            calls
                .iter()
                .any(|call| call.starts_with("rename-noreplace ")),
            !changed
        );
    }
}

/// Two clones of one repository race (`sync.clone` and a second `aterm init`): the one that
/// finds it in place at its origin, before it clones or as it publishes, is done, and its
/// staging goes; the racing worktree is never touched.
#[test]
fn a_clone_that_finds_the_repository_in_place_at_its_origin_is_done() {
    for at_publish in [false, true] {
        let (sys, who, _) = fixture();
        let staged = Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
        let (repo, _, clone_key) = script_clone(&sys, &who, false);
        let destination = Path::new(&who.home).join("new");
        script_existing(&sys, &who, &destination, "new");
        if at_publish {
            sys.on_stat(&clone_key, &destination, directory(who.uid));
            sys.on_write(&clone_key, &destination.join(".git/racer"), b"racing clone");
        } else {
            sys.stat_of(destination.to_str().unwrap(), directory(who.uid));
            sys.file(
                destination.join(".git/racer").to_str().unwrap(),
                b"racing clone",
            );
        }
        assert_eq!(
            clone_one(&sys, &who, "alice", &repo),
            Ok(Placed::InPlace),
            "{at_publish}"
        );
        assert_eq!(
            sys.read_file(&destination.join(".git/racer")).unwrap(),
            b"racing clone"
        );
        assert!(sys.read_file(&destination.join(".git/fixture")).is_err());
        assert!(sys.stat(&staged).is_err(), "{at_publish}");
        assert_eq!(
            sys.calls().iter().any(|call| call.contains(" clone ")),
            at_publish,
            "{:?}",
            sys.calls()
        );
    }
}

/// A racing worktree at this repository's origin whose HEAD names no commit (another
/// `git clone` that has not fetched yet, or one cut off before it did) is no repository in
/// place, before this clone or as it publishes: the pass says it left it untouched, and the
/// next pass's standing read takes it up as the unfinished checkout it is
/// ([`crate::checkout::unborn`]). Before, the race arms accepted the origin and index check
/// alone, which says nothing of a HEAD with no commit, and read it in place.
#[test]
fn a_racing_checkout_with_no_commit_is_never_in_place() {
    for at_publish in [false, true] {
        let (sys, who, _) = fixture();
        let (repo, _, clone_key) = script_clone(&sys, &who, false);
        let destination = Path::new(&who.home).join("new");
        script_existing_head(&sys, &who, &destination, "new", Ran::exit(1, ""));
        if at_publish {
            sys.on_stat(&clone_key, &destination, directory(who.uid));
            sys.on_write(&clone_key, &destination.join(".git/racer"), b"racing clone");
        } else {
            sys.stat_of(destination.to_str().unwrap(), directory(who.uid));
            sys.file(
                destination.join(".git/racer").to_str().unwrap(),
                b"racing clone",
            );
        }
        let placed = clone_one(&sys, &who, "alice", &repo);
        assert!(
            placed
                .as_ref()
                .is_err_and(|why| why.contains("left untouched")),
            "{at_publish}: {placed:?}"
        );
        assert_eq!(
            sys.read_file(&destination.join(".git/racer")).unwrap(),
            b"racing clone",
            "{at_publish}: the racing worktree is never touched"
        );
    }
}

#[test]
fn failure_or_a_destination_created_during_clone_never_overwrites_owner_data() {
    for race in [false, true] {
        let (sys, who, _) = fixture();
        let staged = Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
        if !race {
            sys.on(
                &git(&sys, &who, Some(&staged))
                    .args(["rev-parse", "--is-shallow-repository"])
                    .display(),
                Ran::exit(1, "failure includes secret-like bytes that must not escape"),
            );
        }
        let (repo, _, clone_key) = script_clone(&sys, &who, false);
        let destination = Path::new(&who.home).join("new");
        if race {
            sys.on_stat(&clone_key, &destination, directory(who.uid));
            sys.on_write(&clone_key, &destination.join("owner.txt"), b"racing owner");
        }
        let error = clone_one(&sys, &who, "alice", &repo).unwrap_err();
        assert!(!error.contains("secret-like"));
        assert!(sys.stat(&staged).is_err());
        if race {
            assert_eq!(
                sys.read_file(&destination.join("owner.txt")).unwrap(),
                b"racing owner"
            );
            assert!(sys.read_file(&destination.join(".git/fixture")).is_err());
        } else {
            assert!(sys.stat(&destination).is_err());
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.starts_with("rename-noreplace "))
            );
        }
    }
    for endpoint in [
        "https://mirror.invalid/alice/new.git",
        "https://github.com/other/new.git",
        "https://private-token@github.com/alice/new.git",
        "file:///owner/repository",
        "https://github.com/alice/new.git\nhttps://elsewhere.invalid/new.git",
        "",
    ] {
        let (sys, who, _) = fixture();
        let key = git(&sys, &who, None)
            .args([
                "ls-remote",
                "--get-url",
                "--",
                "https://github.com/alice/new.git",
            ])
            .display();
        sys.on(&key, Ran::ok(endpoint));
        let (repo, _, _) = script_clone(&sys, &who, false);
        let error = clone_one(&sys, &who, "alice", &repo).unwrap_err();
        assert!(!error.contains(endpoint) || endpoint.is_empty());
        assert!(sys.calls().iter().all(|call| !call.contains("--symref")
            && !call.contains(" clone ")
            && !call.contains("scratch allocated")));
    }
    let (sys, who, _) = fixture();
    let key = git(&sys, &who, None)
        .args([
            "ls-remote",
            "--get-url",
            "--",
            "https://github.com/alice/new.git",
        ])
        .display();
    sys.on(&key, Ran::ok("https://github.com/alice/new.git"))
        .on(&key, Ran::ok("https://mirror.invalid/alice/new.git"));
    let (repo, _, _) = script_clone(&sys, &who, false);
    assert!(clone_one(&sys, &who, "alice", &repo).is_err());
    assert!(sys.calls().iter().any(|call| call.contains("--symref")));
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains(" clone ") && !call.contains("scratch allocated"))
    );
}

/// A promisor remote whose name is not UTF-8 makes a partial clone as any other does, and
/// [`verify_integrity`] refuses it: [`partial_listing`] reads the config in the C locale.
/// Under the account's UTF-8 locale git's `.` matches no such byte, the remote went unlisted
/// and the check passed. The controls: a repository with no promisor passes; this one's
/// remote is live (a read of an object the repository lacks, lazy fetches allowed, fetches it
/// from there); and the same listing under the account's own locale misses it (a COULD NOT
/// RUN where it does not, a host whose git matches those bytes there or lacks the locale).
#[test]
fn a_promisor_remote_named_in_bytes_that_are_not_utf8_is_refused() {
    use std::io::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    crate::items::github::need_native_git();
    let sys = Live::new();
    let home = Scratch::new(&sys, "github-promisor-bytes").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = home.path().to_string_lossy().into_owned();
    let run = |at: Option<&Path>, args: &[&str]| {
        text(
            &sys,
            &git(&sys, &who, at)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(crate::checkout::NO_SIDE_RUNS)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args.iter().copied()),
            "isolated local Git fixture failed",
        )
        .unwrap()
    };
    let source = home.path().join("source");
    let repo = home.path().join("repo");
    run(
        None,
        &["init", "--quiet", "--template=", source.to_str().unwrap()],
    );
    sys.write_file(&source.join("code"), b"only the source has this\n", 0o600)
        .unwrap();
    run(Some(&source), &["add", "--", "code"]);
    run(Some(&source), &["commit", "--quiet", "-m", "fixture"]);
    let blob = run(Some(&source), &["rev-parse", "HEAD:code"]);
    let blob = blob.trim();
    run(
        None,
        &["init", "--quiet", "--template=", repo.to_str().unwrap()],
    );
    assert_eq!(
        verify_integrity(&sys, &who, &repo),
        Ok(()),
        "control: a repository with no promisor passes"
    );
    let mut config = std::fs::File::options()
        .append(true)
        .open(repo.join(".git/config"))
        .unwrap();
    config.write_all(b"[remote \"\xff\"]\n\turl = ").unwrap();
    config.write_all(source.as_os_str().as_bytes()).unwrap();
    config.write_all(b"\n\tpromisor = true\n").unwrap();
    drop(config);
    let unlisted = sys
        .run_complete_stdout(&git(&sys, &who, Some(&repo)).args([
            "config",
            "--get-regexp",
            PARTIAL_KEYS,
        ]))
        .unwrap();
    if unlisted.status != Some(1) {
        aterm_uds::host::could_not_run(format!(
            "git under this account's locale lists the remote named in bytes that are not \
             UTF-8 ({}), so the C locale changes nothing here",
            unlisted.describe()
        ));
    }
    let refused = verify_integrity(&sys, &who, &repo).unwrap_err();
    assert!(refused.contains("partial"), "{refused}");
    // The control: git fetches through that remote what the repository lacks.
    let fetched = text(
        &sys,
        &git(&sys, &who, Some(&repo))
            .env("GIT_NO_LAZY_FETCH", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(crate::checkout::NO_SIDE_RUNS)
            .args(["cat-file", "-t", blob]),
        "a read that may fetch lazily",
    )
    .unwrap();
    assert_eq!(fetched, "blob\n");
}

#[test]
fn matching_real_dirty_worktree_on_an_alternate_branch_is_only_read() {
    crate::items::github::need_native_git();
    let sys = Live::new();
    let home = Scratch::new(&sys, "github-repo-fixture").unwrap();
    let mut who = user("fixture", sys.euid());
    who.home = home.path().to_string_lossy().into_owned();
    let path = home.path().join("existing");
    text(
        &sys,
        &git(&sys, &who, None)
            .args(["init", "--template=", "--initial-branch=main"])
            .arg(&path),
        "fixture init",
    )
    .unwrap();
    text(
        &sys,
        // A commit starts automatic maintenance too, whose fork would hold this drain's
        // pipe past git's exit on a loaded machine.
        &git(&sys, &who, Some(&path))
            .args(crate::checkout::NO_SIDE_RUNS)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ]),
        "fixture commit",
    )
    .unwrap();
    text(
        &sys,
        &git(&sys, &who, Some(&path)).args([
            "remote",
            "add",
            "origin",
            "git@github.com:alice/existing.git",
        ]),
        "fixture remote",
    )
    .unwrap();
    text(
        &sys,
        &git(&sys, &who, Some(&path)).args(["switch", "-c", "topic"]),
        "fixture branch",
    )
    .unwrap();
    sys.write_file(&path.join("dirty.txt"), b"local owner edits", 0o600)
        .unwrap();
    let repo = Repo {
        owner: "alice".into(),
        name: "existing".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    };
    verify_existing(&sys, &who, "alice", &repo, &path).unwrap();
    assert_eq!(
        sys.read_file(&path.join("dirty.txt")).unwrap(),
        b"local owner edits"
    );
    assert_eq!(
        git_read(&sys, &who, &path, &["symbolic-ref", "--quiet", "HEAD"])
            .unwrap()
            .trim(),
        "refs/heads/topic"
    );
}

/// A checkout in `~` that a `git clone` left unfinished (killed, or cut off by a power loss)
/// is no repository present, whoever made it. One cut off after it fetched and before it
/// wrote its files has a commit and no index (with its lock or without), and one whose
/// branch names a commit it never fetched has a ref and no commit: each is left untouched
/// under a note. One cut off before it fetched anything has an unborn HEAD, as an empty
/// repository's clone has: no note, and it is the unfinished-checkout path's
/// ([`Partition::unborn`]), which GitHub's advertisement decides ([`observe_unborn`]). The
/// HEAD read fetches nothing. Before, a commit with no index read present.
/// NEGATIVE CONTROL: a finished checkout, its commit and its index there, is present.
#[test]
fn a_checkout_a_clone_left_unfinished_is_not_present() {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Case {
        Finished,
        Unborn,
        CommitNeverFetched,
        CutInCheckout,
        CutBeforeCheckout,
    }
    for case in [
        Case::Finished,
        Case::Unborn,
        Case::CommitNeverFetched,
        Case::CutInCheckout,
        Case::CutBeforeCheckout,
    ] {
        let (sys, who, _) = fixture();
        let path = Path::new(&who.home).join("partial");
        sys.stat_of(path.to_str().unwrap(), directory(who.uid));
        script_worktree(&sys, &who, &path, "partial");
        // `checkout::unborn`'s reads of a HEAD that names no commit here: its branch, and
        // whether a ref names one (exit 1: none; 128: one git cannot read).
        let branch = |reference: Ran| {
            sys.on(
                &git(&sys, &who, Some(&path))
                    .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
                    .display(),
                Ran::ok("main\n"),
            );
            sys.on(
                &git(&sys, &who, Some(&path))
                    .args(["show-ref", "--verify", "--quiet", "refs/heads/main"])
                    .display(),
                reference,
            );
        };
        match case {
            Case::Finished => {
                script_head(&sys, &who, &path, Ran::ok(&format!("{A}\n")));
                script_index(&sys, &path);
            }
            Case::Unborn => {
                script_head(&sys, &who, &path, Ran::exit(1, ""));
                branch(Ran::exit(1, ""));
            }
            Case::CommitNeverFetched => {
                script_head(&sys, &who, &path, Ran::exit(1, ""));
                // As git 2.56 says of a ref to a commit it lacks.
                branch(Ran::exit(
                    128,
                    &format!("fatal: git show-ref: bad ref refs/heads/main ({A})\n"),
                ));
            }
            Case::CutInCheckout => {
                script_head(&sys, &who, &path, Ran::ok(&format!("{A}\n")));
                // The lock sits beside the index the worktree read named.
                let lock = fake_index(&path).with_file_name("index.lock");
                sys.file(lock.to_str().unwrap(), b"");
            }
            Case::CutBeforeCheckout => {
                script_head(&sys, &who, &path, Ran::ok(&format!("{A}\n")));
            }
        }
        let repo = Repo {
            owner: "alice".into(),
            name: "partial".into(),
            default_branch: Some("main".into()),
            size_kib: 0,
            fork: false,
            archived: false,
            required_submodules: Vec::new(),
        };
        let standing = partition(&sys, &who, std::slice::from_ref(&repo)).unwrap();
        let noted = standing
            .notes
            .iter()
            .any(|note| note.starts_with("~/partial"));
        assert_eq!(
            (
                standing.present.len(),
                standing.unborn.len(),
                noted,
                standing.missing.len()
            ),
            (
                usize::from(case == Case::Finished),
                usize::from(case == Case::Unborn),
                matches!(
                    case,
                    Case::CommitNeverFetched | Case::CutInCheckout | Case::CutBeforeCheckout
                ),
                0
            ),
            "{case:?}: {:?}",
            standing.notes
        );
        let head = crate::checkout::head_commit(&sys, &who, &path).display();
        assert!(
            sys.calls().contains(&head) && head.contains("GIT_NO_LAZY_FETCH=1 "),
            "{case:?}: the HEAD read fetches nothing: {:?}",
            sys.calls()
        );
    }
}

#[test]
fn deliberately_shallow_or_partial_matching_existing_checkout_is_preserved_without_integrity_probes()
 {
    let (sys, who, _) = fixture();
    let path = Path::new(&who.home).join("existing");
    script_existing(&sys, &who, &path, "existing");
    sys.refuse_complete_stdout(
        &git(&sys, &who, Some(&path))
            .args(["rev-parse", "--is-shallow-repository"])
            .display(),
        "history intentionally unavailable",
    );
    let repo = Repo {
        owner: "alice".into(),
        name: "existing".into(),
        default_branch: Some("main".into()),
        size_kib: 0,
        fork: false,
        archived: false,
        required_submodules: Vec::new(),
    };
    verify_existing(&sys, &who, "alice", &repo, &path).unwrap();
    assert!(sys.calls().iter().all(|call| !call.contains("shallow")
        && !call.contains("partialclone")
        && !call.contains("fsck")
        && !call.contains("fetch")));
}

#[test]
fn dirty_new_checkout_or_incorrect_tracking_is_never_published() {
    for (args, bad) in [
        (
            vec!["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            " D missing.txt\0",
        ),
        (
            vec!["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            "refs/remotes/origin/other\n",
        ),
        (vec!["config", "--get", "branch.main.remote"], "other\n"),
        (
            vec!["config", "--get", "branch.main.merge"],
            "refs/heads/other\n",
        ),
    ] {
        let (sys, who, _) = fixture();
        let staged = Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
        sys.on(
            &git(&sys, &who, Some(&staged)).args(args).display(),
            Ran::ok(bad),
        );
        let (repo, _, _) = script_clone(&sys, &who, false);
        assert!(clone_one(&sys, &who, "alice", &repo).is_err());
        assert!(sys.stat(&Path::new(&who.home).join("new")).is_err());
        assert!(sys.stat(&staged).is_err());
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.starts_with("rename-noreplace "))
        );
    }
}
