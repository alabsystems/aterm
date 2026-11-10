// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::repo_work::PROBE_LIMIT;
use crate::harness::repo_work::tests::scratch_git;

/// A clone at `<home>/<name>` whose `origin` is `url`, with one commit.
fn clone_at(home: &Path, name: &str, url: &str) -> PathBuf {
    let dir = home.join(name);
    std::fs::create_dir_all(&dir).expect("checkout");
    scratch_git(&dir, &["init", "-q", "."]);
    std::fs::write(dir.join("a.txt"), "one\n").expect("a.txt");
    scratch_git(&dir, &["add", "a.txt"]);
    scratch_git(&dir, &["commit", "-q", "-m", "one"]);
    scratch_git(&dir, &["remote", "add", "origin", url]);
    dir
}

fn read(home: &Path) -> HomeRepos {
    read_home(home, PROBE_LIMIT, &|| false).expect("the home lists")
}

/// The reflog at `path` with its last entry's recorded time made `secs` (the file's own time is
/// the rewrite's, later than any fixture's).
fn last_logged_at(path: &Path, secs: u64) {
    let text = std::fs::read_to_string(path).expect("a reflog");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let last = lines.last_mut().expect("an entry");
    let entry = {
        // An entry with no message (an `update-ref` without `-m`) has no tab.
        let (who, message) = last
            .split_once('\t')
            .map_or((last.as_str(), None), |(who, message)| (who, Some(message)));
        let mut words: Vec<String> = who.split(' ').map(str::to_owned).collect();
        let at = words.len() - 2;
        words[at] = secs.to_string();
        let who = words.join(" ");
        message.map_or(who.clone(), |message| format!("{who}\t{message}"))
    };
    *last = entry;
    std::fs::write(path, lines.join("\n") + "\n").expect("the reflog rewritten");
}

fn set_modified(path: &Path, at: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(at))
        .expect("a file's time");
}

fn unix_ms(at: std::time::SystemTime) -> u64 {
    u64::try_from(
        at.duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_millis(),
    )
    .expect("fits")
}

/// A HOME LISTS EACH CLONE OF ITS OWN GITHUB REPOSITORY, BY NAME: a checkout behind its origin's
/// main by one commit as of its fetch, with an untracked file, and a clean one with no main ref.
/// Left out, each by its own rule: a linked worktree (a `.git` file), a clone of another
/// repository than its folder names, one of no GitHub repository, a hidden folder, one macOS
/// guards (`~/Documents`, never looked into) and a link to a clone.
#[test]
fn a_home_lists_each_clone_of_its_github_repository_by_name() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let trust = clone_at(&home, "trust", "https://github.com/acme/trust.git");
    let first = scratch_git(&trust, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    std::fs::write(trust.join("a.txt"), "two\n").expect("a.txt");
    scratch_git(&trust, &["commit", "-q", "-am", "two"]);
    let second = scratch_git(&trust, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    scratch_git(&trust, &["update-ref", "refs/remotes/origin/main", &second]);
    scratch_git(
        &trust,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    scratch_git(&trust, &["reset", "-q", "--hard", &first]);
    std::fs::write(trust.join("untracked.txt"), "new\n").expect("untracked");
    let fetched = std::time::UNIX_EPOCH + Duration::from_secs(1_793_000_000);
    std::fs::write(
        trust.join(".git/FETCH_HEAD"),
        format!("{second}\t\tbranch 'main' of https://github.com/acme/trust\n"),
    )
    .expect("FETCH_HEAD, as a fetch of origin's main writes it");
    std::fs::File::options()
        .write(true)
        .open(trust.join(".git/FETCH_HEAD"))
        .and_then(|f| f.set_modified(fetched))
        .expect("FETCH_HEAD's time");
    let reflog = trust.join(".git/logs/refs/remotes/origin/main");
    last_logged_at(&reflog, 1_793_000_000 - 60);
    let aterm = clone_at(&home, "aterm", "git@github.com:acme/aterm");
    let aterm_head = scratch_git(&aterm, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();

    scratch_git(
        &trust,
        &[
            "worktree",
            "add",
            "-q",
            &home.join("trust-wt").display().to_string(),
        ],
    );
    clone_at(&home, "fork", "https://github.com/acme/other.git");
    clone_at(
        &home,
        "local",
        &tmp.path().join("remote.git").display().to_string(),
    );
    clone_at(&home, ".dotfiles", "https://github.com/acme/.dotfiles.git");
    clone_at(&home, "Documents", "https://github.com/acme/Documents.git");
    std::os::unix::fs::symlink(&trust, home.join("linked")).expect("a link");

    let found = read(&home);
    assert_eq!(found.cut, 0);
    assert_eq!(
        found.repos,
        vec![
            HomeRepo {
                name: "aterm".to_owned(),
                path: home.join("aterm"),
                head: Some(aterm_head),
                branch: Some("main".to_owned()),
                main_ref: None,
                behind: None,
                fetched_ms: None,
                dirty: Some(false),
            },
            HomeRepo {
                name: "trust".to_owned(),
                path: home.join("trust"),
                head: Some(first),
                branch: Some("main".to_owned()),
                main_ref: Some("origin/main".to_owned()),
                behind: Some(1),
                fetched_ms: Some(unix_ms(fetched)),
                dirty: Some(true),
            },
        ]
    );
    assert_eq!(found.named("TRUST").map(|r| r.name.as_str()), Some("trust"));
    assert!(found.named("trust-wt").is_none());

    // Negative control: the reflog's last entry is the fetch's time where FETCH_HEAD is gone (a
    // clone that never fetched writes none), its recorded time and not the file's (a rewrite's,
    // later), unless the file's is earlier (an entry dated ahead of its writing).
    std::fs::remove_file(trust.join(".git/FETCH_HEAD")).expect("rm FETCH_HEAD");
    let fetched_ms = |home: &Path| read(home).named("trust").and_then(|r| r.fetched_ms);
    assert_eq!(
        fetched_ms(&home),
        Some(unix_ms(fetched - Duration::from_secs(60)))
    );
    set_modified(&reflog, fetched - Duration::from_secs(120));
    assert_eq!(
        fetched_ms(&home),
        Some(unix_ms(fetched - Duration::from_secs(120)))
    );
}

/// A READ THAT CANNOT SAY WHAT THE HOME HOLDS SAYS NOTHING: with new starts held back, no
/// checkout's origin can be read, so the read is `None`, never an empty home. Negative control:
/// the same home, nothing held, lists its clone.
#[test]
fn a_read_held_back_says_nothing_of_the_home() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    clone_at(&home, "trust", "https://github.com/acme/trust");
    assert_eq!(read_home(&home, PROBE_LIMIT, &|| true), None);
    assert_eq!(read(&home).repos.len(), 1);
    assert_eq!(read_home(Path::new("home"), PROBE_LIMIT, &|| false), None);
    assert_eq!(
        read_home(&tmp.path().join("missing"), PROBE_LIMIT, &|| false),
        None
    );
}

/// Past [`HOME_REPOS_MAX`] clones the rest are counted, not read: a name the read does not list
/// may be among them.
#[test]
fn clones_past_the_most_a_read_lists_are_counted() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    for n in 0..=HOME_REPOS_MAX {
        let dir = home.join(format!("r{n:03}"));
        std::fs::create_dir_all(&dir).expect("a folder");
        scratch_git(&dir, &["init", "-q", "."]);
    }
    let found = read(&home);
    assert_eq!(found.cut, 1);
    assert!(found.repos.is_empty(), "none of them names an origin");
}

#[test]
fn a_github_origin_names_its_repository() {
    for (url, name) in [
        ("https://github.com/acme/trust", Some("trust")),
        ("https://github.com/acme/trust.git", Some("trust")),
        ("https://github.com/acme/trust/", Some("trust")),
        ("git@github.com:acme/trust.git", Some("trust")),
        ("ssh://git@github.com/acme/trust.git", Some("trust")),
        ("ssh://git@github.com:22/acme/trust", Some("trust")),
        ("https://token@github.com/acme/trust.git", None),
        ("https://gitlab.com/acme/trust.git", None),
        ("https://github.com/trust", None),
        ("https://github.com/acme/a/b", None),
        ("/srv/trust.git", None),
    ] {
        assert_eq!(github_repo(url).map(|(_, named)| named), name, "{url}");
    }
    assert!(repo_name_ok("trust-sha2_1.x"));
    for bad in ["", ".hidden", "a b", "a/b", "é", &"a".repeat(101)] {
        assert!(!repo_name_ok(bad), "{bad:?}");
    }
}

/// [`refresh`] starts one read beside its caller, and [`latest`] then has it, for that home
/// alone; a second ask within [`REFRESH`] starts none.
#[test]
fn a_refresh_reads_the_home_beside_its_caller() {
    let _one = one_test_at_a_time();
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    clone_at(&home, "trust", "https://github.com/acme/trust");
    assert_eq!(latest(&home), None, "nothing read yet");
    fn never() -> bool {
        false
    }
    // A read the test before started may not have let go yet: ask until this one starts.
    let read = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the home read beside the caller",
        || {
            refresh(&home, never);
            latest(&home)
        },
    );
    assert_eq!(read.repos.len(), 1);
    assert_eq!(latest(&tmp.path().join("other")), None, "another home");
    std::fs::remove_dir_all(home.join("trust")).expect("rm");
    refresh(&home, never);
    assert_eq!(
        latest(&home).map(|r| r.repos.len()),
        Some(1),
        "a read within REFRESH stands"
    );
}

/// A HOME ON A VOLUME THE READ NEVER ENTERS SAYS NOTHING, AND A CLONE ON ONE IS LEFT OUT, BEFORE
/// ANY LOOK: a volume table read in part, or one that puts the home on a network volume, gives no
/// read at all; a network or automounted volume mounted at a folder of the home is never looked
/// into. Negative control: the same home with the table as read lists every clone.
#[test]
fn a_home_on_a_volume_the_read_never_enters_says_nothing_or_leaves_it_out() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    clone_at(&home, "trust", "https://github.com/acme/trust");
    clone_at(&home, "share", "https://github.com/acme/share");
    let table = aterm_sysprobe::volumes::table();
    assert!(table.complete, "this machine's table reads whole");
    let read =
        |table: &aterm_sysprobe::volumes::Table| read_home_in(&home, table, PROBE_LIMIT, &|| false);
    let names = |found: Option<HomeRepos>| {
        found.map(|found| found.repos.into_iter().map(|r| r.name).collect::<Vec<_>>())
    };
    assert_eq!(
        names(read(&table)),
        Some(vec!["share".to_owned(), "trust".to_owned()])
    );
    let mounted = |on: PathBuf, local: bool, automounted: bool| {
        let mut table = table.clone();
        table.mounts.push(aterm_sysprobe::volumes::Mount {
            on,
            from: "//nas/share".to_owned(),
            fstype: "smbfs".to_owned(),
            local,
            automounted,
            removable: false,
            read_only: false,
            container: None,
        });
        table
    };
    // A volume table names each mount where its links resolve.
    let canonical = std::fs::canonicalize(&home).expect("the home, canonical");
    for (local, automounted) in [(false, false), (true, true)] {
        assert_eq!(
            names(read(&mounted(canonical.join("share"), local, automounted))),
            Some(vec!["trust".to_owned()]),
            "local={local} automounted={automounted}"
        );
        assert_eq!(read(&mounted(canonical.clone(), local, automounted)), None);
    }
    let part = aterm_sysprobe::volumes::Table {
        complete: false,
        ..table.clone()
    };
    assert_eq!(read(&part), None, "a table read in part");

    // A home that is a link onto such a volume is never listed: its links resolve first, each
    // step judged local. Negative control: the same link onto a local folder lists its clones,
    // each at the home's path as written.
    let far = tmp.path().join("far");
    std::fs::create_dir_all(&far).expect("far");
    clone_at(&far, "trust", "https://github.com/acme/trust");
    let linked = tmp.path().join("linked");
    std::os::unix::fs::symlink(&far, &linked).expect("a linked home");
    let read_linked = |table: &aterm_sysprobe::volumes::Table| {
        read_home_in(&linked, table, PROBE_LIMIT, &|| false)
    };
    let canonical_far = std::fs::canonicalize(&far).expect("far, canonical");
    for (local, automounted) in [(false, false), (true, true)] {
        assert_eq!(
            read_linked(&mounted(canonical_far.clone(), local, automounted)),
            None,
            "local={local} automounted={automounted}"
        );
    }
    let found = read_linked(&table).expect("a linked home on a local volume lists");
    assert_eq!(
        found
            .repos
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>(),
        vec![linked.join("trust")]
    );
}

/// A FETCH OF ANOTHER REMOTE OR BRANCH DATES NO FETCH OF ORIGIN'S MAIN: `FETCH_HEAD`'s time counts
/// only where it records `main` from the repository `origin` names (as git spells either URL),
/// else the main ref's reflog dates it.
#[test]
fn only_a_fetch_of_origins_main_dates_the_checkouts_fetch() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let trust = clone_at(&home, "trust", "git@github.com:acme/trust.git");
    let head = scratch_git(&trust, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    scratch_git(&trust, &["update-ref", "refs/remotes/origin/main", &head]);
    scratch_git(
        &trust,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let at = |secs: u64| std::time::UNIX_EPOCH + Duration::from_secs(secs);
    last_logged_at(
        &trust.join(".git/logs/refs/remotes/origin/main"),
        1_793_000_000,
    );
    let fetched_with = |sha: &str, line: &str| {
        let path = trust.join(".git/FETCH_HEAD");
        std::fs::write(&path, format!("{sha}\t\t{line}\n")).expect("FETCH_HEAD");
        set_modified(&path, at(1_793_100_000));
        read(&home).named("trust").and_then(|r| r.fetched_ms)
    };
    let fetch = Some(unix_ms(at(1_793_100_000)));
    let reflog_only = Some(unix_ms(at(1_793_000_000)));
    for (line, want) in [
        ("branch 'main' of github.com:acme/trust", fetch),
        ("branch 'main' of https://github.com/ACME/trust", fetch),
        ("branch 'feature' of github.com:acme/trust", reflog_only),
        ("branch 'main' of github.com:upstream/trust", reflog_only),
        (
            "branch 'main' of https://gitlab.com/acme/trust",
            reflog_only,
        ),
        ("'refs/pull/7/head' of github.com:acme/trust", reflog_only),
    ] {
        assert_eq!(fetched_with(&head, line), want, "{line}");
    }
    // A fetch of origin's main at another commit than the main ref holds now (the ref moved
    // since, by a push or by hand) dates no fetch of what the ref holds.
    let other = "0".repeat(head.len());
    assert_eq!(
        fetched_with(&other, "branch 'main' of github.com:acme/trust"),
        reflog_only
    );
    assert_eq!(
        fetched_with(
            &head.to_ascii_uppercase(),
            "branch 'main' of github.com:acme/trust"
        ),
        fetch
    );
}

/// A read its clock puts in the future (the clock stepped back since) is read again at once, so
/// the survey never keeps saying it until the clock catches up.
#[test]
fn a_read_dated_in_the_future_is_read_again() {
    let _one = one_test_at_a_time();
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    clone_at(&home, "trust", "https://github.com/acme/trust");
    let now = aterm_time::unix_millis().expect("a clock");
    let ahead = now + 2 * 24 * 60 * 60 * 1000;
    plant_latest(
        &home,
        HomeRepos {
            read_ms: ahead,
            repos: Vec::new(),
            cut: 0,
        },
    );
    fn never() -> bool {
        false
    }
    // A read another test started may still end: ask until this one starts and ends.
    let read = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a new read of the home",
        || {
            refresh(&home, never);
            latest(&home).filter(|read| read.read_ms < ahead)
        },
    );
    assert_eq!(read.repos.len(), 1);
}

/// A [`read_home_in`] pass resolves `remote.origin.url` + `core.excludesFile` in one `git config`
/// call, resolves `HEAD` + `origin/HEAD` + `origin/main` + `origin/master` in one `git
/// for-each-ref` call, skips `rev-list --count` when `HEAD == main_head`, and passes the known
/// `.git/index` path to [`work_status_in`]: a warm up-to-date checkout spawns 5 `git` processes
/// instead of 11 (`103` vs `221` across the 21-checkout fixture).
#[test]
fn home_repos_pass_avoids_redundant_git_spawns_across_checkouts() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    for idx in 0..18 {
        let name = format!("repo-{idx:02}");
        let dir = clone_at(&home, &name, &format!("https://github.com/acme/{name}.git"));
        let head = scratch_git(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        scratch_git(&dir, &["update-ref", "refs/remotes/origin/main", &head]);
        scratch_git(
            &dir,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
    }
    let lagging = clone_at(&home, "lagging", "https://github.com/acme/lagging.git");
    let first = scratch_git(&lagging, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    std::fs::write(lagging.join("a.txt"), "two\n").expect("a.txt");
    scratch_git(&lagging, &["commit", "-q", "-am", "two"]);
    let second = scratch_git(&lagging, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    scratch_git(
        &lagging,
        &["update-ref", "refs/remotes/origin/main", &second],
    );
    scratch_git(&lagging, &["reset", "-q", "--hard", &first]);
    std::fs::write(lagging.join("dirty.txt"), "new\n").expect("dirty");
    clone_at(&home, "unpushed", "https://github.com/acme/unpushed.git");
    clone_at(&home, "non-matching", "https://github.com/acme/other.git");

    let table = aterm_sysprobe::volumes::table();
    // First pass warms the refreshed private index copies (`private_index`).
    let (warmed, cold_starts) =
        read_home_in_counted(&home, &table, PROBE_LIMIT, &|| false).expect("cold pass");
    assert_eq!(warmed.repos.len(), 20);
    assert_eq!(warmed.named("repo-00").and_then(|r| r.behind), Some(0));
    assert_eq!(warmed.named("lagging").and_then(|r| r.behind), Some(1));
    assert_eq!(warmed.named("lagging").and_then(|r| r.dirty), Some(true));
    assert_eq!(warmed.named("unpushed").and_then(|r| r.behind), None);
    assert_eq!(
        cold_starts, 123,
        "cold pass spawns 123 git processes (down from 241)"
    );

    for _ in 0..3 {
        let (found, warm_starts) =
            read_home_in_counted(&home, &table, PROBE_LIMIT, &|| false).expect("warm pass");
        assert_eq!(found.repos, warmed.repos);
        assert_eq!(
            warm_starts, 103,
            "warm pass spawns 103 git processes across 21 checkouts (down from 221)"
        );
    }

    // Edge cases: detached HEAD, custom `origin/HEAD -> refs/remotes/origin/trunk`, and a
    // repository-local `core.excludesFile` (caught by `local_config` so `work_status_in` skips
    // `global_excludes` and honours the repository's own exclude file).
    let edge_home = tmp.path().join("edge-home");
    std::fs::create_dir_all(&edge_home).expect("edge-home");
    let custom = clone_at(&edge_home, "custom", "https://github.com/acme/custom.git");
    let custom_head = scratch_git(&custom, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    scratch_git(
        &custom,
        &["update-ref", "refs/remotes/origin/trunk", &custom_head],
    );
    scratch_git(
        &custom,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    scratch_git(&custom, &["checkout", "-q", "--detach", &custom_head]);
    let exclude_file = tmp.path().join("custom.exclude");
    std::fs::write(&exclude_file, "*.ignored\n").expect("exclude");
    std::fs::write(custom.join("secret.ignored"), "ignored\n").expect("secret");
    scratch_git(
        &custom,
        &[
            "config",
            "core.excludesFile",
            &exclude_file.display().to_string(),
        ],
    );
    let _ = read_home_in_counted(&edge_home, &table, PROBE_LIMIT, &|| false).expect("edge cold");
    let (edge, edge_warm_starts) =
        read_home_in_counted(&edge_home, &table, PROBE_LIMIT, &|| false).expect("edge warm");
    let repo = edge.named("custom").expect("custom repo");
    assert_eq!(repo.head.as_deref(), Some(custom_head.as_str()));
    assert_eq!(repo.branch, None, "detached HEAD reports no branch");
    assert_eq!(repo.main_ref.as_deref(), Some("origin/trunk"));
    assert_eq!(repo.behind, Some(0));
    assert_eq!(
        repo.dirty,
        Some(false),
        "repository-local core.excludesFile ignores secret.ignored"
    );
    assert_eq!(
        edge_warm_starts, 6,
        "--version + local_config + symbolic-ref + for-each-ref + rev-parse HEAD + status (global_excludes skipped)"
    );
}
