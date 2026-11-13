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

/// THE READS [`refresh`] STARTS KEEP GIT'S ANSWERS FROM ONE TO THE NEXT: the node and the window
/// read their home through `refresh` alone, and its second read of a home nothing changed
/// starts `--version` and one status a checkout, where its first asked everything. The second
/// read is started by giving the first an age past [`REFRESH`].
///
/// NEGATIVE CONTROL (2026-11-12): with `refresh` reading under a new `Kept` each time, or not
/// putting the one it took back in `HOME_KEPT`, the second read starts 7 gits (`left: Some(7),
/// right: Some(3)`).
#[test]
fn the_reads_a_refresh_starts_keep_gits_answers_from_one_to_the_next() {
    let _one = one_test_at_a_time();
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    clone_at(&home, "trust", "https://github.com/acme/trust");
    clone_at(&home, "ty", "https://github.com/acme/ty");
    fn never() -> bool {
        false
    }
    // A read the test before started may not have let go yet: ask until this one starts.
    let first = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the home read beside the caller",
        || {
            refresh(&home, never);
            latest(&home)
        },
    );
    assert_eq!(first.repos.len(), 2);
    assert_eq!(
        latest_starts(&home),
        Some((first.read_ms, 9)),
        "--version, and 4 a clone (config, for-each-ref, update-index, status)"
    );
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the first read lets go",
        || (!reading_now()).then_some(()),
    );
    // Read at the epoch: older than REFRESH, so the next ask reads the home again.
    plant_latest(
        &home,
        HomeRepos {
            read_ms: 0,
            ..first.clone()
        },
    );
    let second = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a second read of the home",
        || {
            refresh(&home, never);
            latest(&home).filter(|read| read.read_ms > 0)
        },
    );
    assert_eq!(second.repos, first.repos);
    assert_eq!(
        latest_starts(&home).map(|(_, starts)| starts),
        Some(3),
        "--version and one status a checkout"
    );
}

/// The names a read lists.
fn names(found: &HomeRepos) -> Vec<&str> {
    found.repos.iter().map(|repo| repo.name.as_str()).collect()
}

/// `path` rewritten where it stands with every `from` made `to`, of the same length, and its
/// modification time put back: the same file, as long and as old, holding other bytes.
fn rewritten_in_place(path: &Path, from: &str, to: &str) {
    assert_eq!(from.len(), to.len(), "the file keeps its length");
    let before = std::fs::metadata(path).expect("the file");
    let text = std::fs::read_to_string(path).expect("its text");
    assert!(text.contains(from), "{from:?} in {}", path.display());
    std::fs::write(path, text.replace(from, to)).expect("rewritten");
    set_modified(path, before.modified().expect("its time"));
    let after = std::fs::metadata(path).expect("the file");
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().ok(), before.modified().ok());
}

/// AN UNCHANGED CHECKOUT STARTS ONE GIT AT A PASS, ITS STATUS: what git said of its configuration,
/// its refs and its distance behind origin's main is kept under the exact bytes git answered from
/// ([`kept`]), and a pass that reads those bytes again and finds them standing asks git nothing
/// but the working tree's status. Over 21 folders with a `.git` (20 clones of their own GitHub
/// repository), a first pass starts 83 gits and each later one 21 (`--version` and 20 statuses),
/// where every pass started 63 before.
///
/// NEGATIVE CONTROLS, each a change no file time or length shows, or a rule that keeps nothing:
/// - a git that says another version of itself is asked everything again (63 starts);
/// - a pass held back after its first start says nothing, though every answer is kept (`kept_or`
///   giving a kept answer without [`Reads::start_by`] lists the home);
/// - a main ref moved inside `packed-refs` alone is read again (a refs reading without
///   `packed-refs` keeps the checkout one commit behind);
/// - a configuration rewritten where it stands, as long and as old, to name another repository
///   takes the checkout off the list (a reading that takes a file's length for its bytes keeps
///   it listed);
/// - a configuration that includes another file is asked at every pass, so an edit to the
///   included file shows at the next (kept, the checkout stays listed);
/// - a repository with a replace ref is asked for its refs at every pass.
#[test]
fn an_unchanged_checkout_starts_one_git_at_a_pass() {
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
    let mut kept = Kept::default();
    let mut pass =
        |held: &dyn Fn() -> bool| read_home_in_counted(&home, &table, PROBE_LIMIT, held, &mut kept);
    // The first pass asks everything, and makes each refreshed private index copy.
    let (warmed, cold_starts) = pass(&|| false).expect("cold pass");
    assert_eq!(warmed.repos.len(), 20);
    assert_eq!(warmed.named("repo-00").and_then(|r| r.behind), Some(0));
    assert_eq!(warmed.named("lagging").and_then(|r| r.behind), Some(1));
    assert_eq!(warmed.named("lagging").and_then(|r| r.dirty), Some(true));
    assert_eq!(warmed.named("unpushed").and_then(|r| r.behind), None);
    assert_eq!(
        cold_starts, 83,
        "--version, 4 a clone (config, for-each-ref, update-index, status), 1 more for the one \
         behind (rev-list), 1 for the folder that is no clone of its name (config)"
    );
    for _ in 0..3 {
        let (found, starts) = pass(&|| false).expect("warm pass");
        assert_eq!(found.repos, warmed.repos);
        assert_eq!(starts, 21, "--version and one status a listed checkout");
    }

    // Another git: everything it said is asked again, as every pass asked before answers were
    // kept (3 a clone, 4 for the one behind, 1 for the folder that is no clone of its name).
    drop(pass);
    kept.begin(&home, "git version 0.0.0 (another)");
    let mut pass =
        |held: &dyn Fn() -> bool| read_home_in_counted(&home, &table, PROBE_LIMIT, held, &mut kept);
    let (found, starts) = pass(&|| false).expect("another git's pass");
    assert_eq!(found.repos, warmed.repos);
    assert_eq!(starts, 63);
    assert_eq!(pass(&|| false).map(|(_, starts)| starts), Some(21));

    // Held back after its first start (`--version`), with every answer kept: nothing is said.
    let asked = Cell::new(0_u32);
    let held_after_one = || {
        asked.set(asked.get() + 1);
        asked.get() > 1
    };
    assert_eq!(pass(&held_after_one), None, "a pass held back says nothing");
    let (found, starts) = pass(&|| false).expect("the pass after it");
    assert_eq!((found.repos, starts), (warmed.repos.clone(), 21));

    // The refs packed: other files hold the same refs, so git is asked again and says the same.
    scratch_git(&lagging, &["pack-refs", "--all"]);
    let (found, starts) = pass(&|| false).expect("packed");
    assert_eq!(found.named("lagging"), warmed.named("lagging"));
    assert_eq!(starts, 23, "for-each-ref and rev-list again for the one");
    assert_eq!(pass(&|| false).map(|(_, starts)| starts), Some(21));
    // Origin's main moved to the checkout's own commit and packed again: only the bytes of
    // `packed-refs` differ from the pass before.
    scratch_git(
        &lagging,
        &["update-ref", "refs/remotes/origin/main", &first],
    );
    scratch_git(&lagging, &["pack-refs", "--all"]);
    assert!(!lagging.join(".git/refs/remotes/origin/main").exists());
    let (found, starts) = pass(&|| false).expect("main moved in packed-refs");
    assert_eq!(found.named("lagging").and_then(|r| r.behind), Some(0));
    assert_eq!(starts, 22, "for-each-ref again for the one");
    assert_eq!(pass(&|| false).map(|(_, starts)| starts), Some(21));

    // A configuration rewritten where it stands, as long and as old, naming another repository.
    let config = home.join("repo-01/.git/config");
    rewritten_in_place(&config, "acme/repo-01.git", "acme/repo-91.git");
    let (found, starts) = pass(&|| false).expect("origin renamed in place");
    assert!(found.named("repo-01").is_none(), "no clone of its name now");
    assert_eq!(found.repos.len(), 19);
    assert_eq!(starts, 21, "--version, its config, 19 statuses");
    rewritten_in_place(&config, "acme/repo-91.git", "acme/repo-01.git");
    let (found, starts) = pass(&|| false).expect("origin named back");
    assert_eq!(found.named("repo-01"), warmed.named("repo-01"));
    assert_eq!(starts, 22, "--version, its config, 20 statuses");

    // A configuration that includes another file: git reads a file no reading here takes, so
    // nothing is kept of the checkout's configuration or its refs.
    let included = home.join("repo-02/.git/origin.cfg");
    scratch_git(
        &home.join("repo-02"),
        &["config", "--unset", "remote.origin.url"],
    );
    scratch_git(
        &home.join("repo-02"),
        &["config", "include.path", "origin.cfg"],
    );
    std::fs::write(
        &included,
        "[remote \"origin\"]\n\turl = https://github.com/acme/repo-02.git\n",
    )
    .expect("the included file");
    for _ in 0..2 {
        let (found, starts) = pass(&|| false).expect("an including checkout");
        assert_eq!(found.named("repo-02"), warmed.named("repo-02"));
        assert_eq!(starts, 23, "its config and for-each-ref at every pass");
    }
    std::fs::write(
        &included,
        "[remote \"origin\"]\n\turl = https://github.com/acme/repo-92.git\n",
    )
    .expect("the included file, naming another repository");
    let (found, starts) = pass(&|| false).expect("the included origin renamed");
    assert!(found.named("repo-02").is_none(), "no clone of its name now");
    assert_eq!(starts, 21, "--version, its config, 19 statuses");

    // A replace ref: what git says of a history is no longer for its refs' files alone to say.
    let head = warmed
        .named("repo-03")
        .and_then(|r| r.head.clone())
        .expect("its head");
    let other = scratch_git(
        &home.join("repo-03"),
        &["commit-tree", "-m", "another", &format!("{head}^{{tree}}")],
    );
    scratch_git(
        &home.join("repo-03"),
        &["update-ref", &format!("refs/replace/{head}"), other.trim()],
    );
    for _ in 0..2 {
        let (found, starts) = pass(&|| false).expect("a replace ref");
        assert_eq!(found.named("repo-03"), warmed.named("repo-03"));
        assert_eq!(
            starts, 22,
            "--version, repo-02's config, repo-03's for-each-ref, 19 statuses"
        );
    }
}

/// THE KEPT ANSWERS OF A DETACHED HEAD AND OF EACH PLACE AN EXCLUDES FILE IS SET: a checkout with
/// `HEAD` detached, a custom `origin/HEAD -> origin/trunk` and its own `core.excludesFile` starts
/// only its status at a later pass, and so does one whose excludes file is Git's default
/// (`~/.config/git/ignore`) or the person's (`~/.gitconfig`, whose `--path` expansion is kept
/// under the person's configuration); a global `[remote "origin"]` never reads as a checkout's
/// own origin. NEGATIVE CONTROLS: `HEAD` detached at another commit is read again (kept under
/// refs alone, the head stays the earlier commit), and the person's configuration naming another
/// excludes file is read again (kept, the ignored file stays ignored).
#[test]
fn a_detached_head_and_each_excludes_setting_are_kept_under_their_bytes() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("edge-home");
    std::fs::create_dir_all(&home).expect("edge-home");
    let custom = clone_at(&home, "custom", "https://github.com/acme/custom.git");
    let first = scratch_git(&custom, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    std::fs::write(custom.join("a.txt"), "two\n").expect("a.txt");
    scratch_git(&custom, &["commit", "-q", "-am", "two"]);
    let second = scratch_git(&custom, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    scratch_git(
        &custom,
        &["update-ref", "refs/remotes/origin/trunk", &second],
    );
    scratch_git(
        &custom,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    scratch_git(&custom, &["checkout", "-q", "--detach", &second]);
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
    let table = aterm_sysprobe::volumes::table();
    let mut kept = Kept::default();
    let mut pass =
        || read_home_in_counted(&home, &table, PROBE_LIMIT, &|| false, &mut kept).expect("a pass");

    let (_, cold_starts) = pass();
    assert_eq!(
        cold_starts, 6,
        "--version, config, for-each-ref, rev-parse HEAD, update-index, status"
    );
    let (edge, starts) = pass();
    let repo = edge.named("custom").expect("custom repo");
    assert_eq!(repo.head.as_deref(), Some(second.as_str()));
    assert_eq!(repo.branch, None, "detached HEAD reports no branch");
    assert_eq!(repo.main_ref.as_deref(), Some("origin/trunk"));
    assert_eq!(repo.behind, Some(0));
    assert_eq!(
        repo.dirty,
        Some(false),
        "repository-local core.excludesFile ignores secret.ignored"
    );
    assert_eq!(starts, 2, "--version and status");

    // Detached at another commit: `HEAD`'s bytes moved, so its commit and its distance are read
    // again.
    scratch_git(&custom, &["checkout", "-q", "--detach", &first]);
    let (edge, starts) = pass();
    let repo = edge.named("custom").expect("custom repo");
    assert_eq!(repo.head.as_deref(), Some(first.as_str()));
    assert_eq!(repo.behind, Some(1));
    assert_eq!(
        starts, 6,
        "--version, for-each-ref, rev-parse HEAD, rev-list, update-index, status"
    );
    assert_eq!(pass().1, 2, "--version and status");

    // Git's default excludes file (`~/.config/git/ignore`): no git says where it is.
    scratch_git(&custom, &["checkout", "-q", "main"]);
    scratch_git(&custom, &["config", "--unset", "core.excludesFile"]);
    let xdg_git = home.join(".config/git");
    std::fs::create_dir_all(&xdg_git).expect("xdg git");
    std::fs::write(xdg_git.join("ignore"), "*.ignored\n").expect("xdg ignore");
    let (default_global, _) = pass();
    assert_eq!(
        default_global.named("custom").and_then(|r| r.dirty),
        Some(false)
    );
    assert_eq!(pass().1, 2, "--version and status");

    // The person's own (`~/.gitconfig`): its `--path` expansion is asked once.
    std::fs::remove_file(xdg_git.join("ignore")).expect("rm xdg ignore");
    std::fs::write(home.join("home.exclude"), "*.ignored\n").expect("home.exclude");
    std::fs::write(home.join("none.exclude"), "*.nothing\n").expect("none.exclude");
    std::fs::write(
        home.join(".gitconfig"),
        "[core]\n\texcludesFile = ~/home.exclude\n[remote \"origin\"]\n\turl = https://github.com/acme/decoy.git\n",
    )
    .expect("gitconfig");
    let decoy = home.join("decoy");
    std::fs::create_dir_all(&decoy).expect("decoy");
    scratch_git(&decoy, &["init", "-q", "."]);
    let (configured_global, starts) = pass();
    assert_eq!(
        configured_global.named("custom").and_then(|r| r.dirty),
        Some(false)
    );
    assert_eq!(
        names(&configured_global),
        ["custom"],
        "global remote.origin.url never counts as a checkout's own origin"
    );
    assert_eq!(
        starts, 5,
        "--version, decoy's config, custom's config, the person's --path, status"
    );
    assert_eq!(pass().1, 2, "--version and status");

    // The person's configuration naming another excludes file, as long as the first.
    rewritten_in_place(&home.join(".gitconfig"), "home.exclude", "none.exclude");
    let (renamed, starts) = pass();
    assert_eq!(
        renamed.named("custom").and_then(|r| r.dirty),
        Some(true),
        "secret.ignored is no longer ignored"
    );
    assert_eq!(
        starts, 5,
        "--version, decoy's config, custom's config, the person's --path, status"
    );
    assert_eq!(pass().1, 2, "--version and status");
}

/// A READING VOUCHES ONLY FOR WHAT IT CAN READ: the evidence for a checkout's refs is the same
/// at two looks while its files stand, and there is none (so nothing is kept, and git is asked at
/// every pass) where git would read what the reading does not: a ref name that is no plain path,
/// a symbolic ref to a symbolic ref, another repository's objects, or another's `.git`. Each is
/// its own negative control: with the file gone again the reading is the first one.
#[test]
fn a_reading_vouches_only_for_refs_it_can_read() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let dir = clone_at(&home, "trust", "https://github.com/acme/trust.git");
    let root = std::fs::canonicalize(&dir).expect("the checkout, canonical");
    let git_dir = root.join(".git");
    let head = scratch_git(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    scratch_git(&dir, &["update-ref", "refs/remotes/origin/main", &head]);
    let asked = ["refs/remotes/origin/HEAD", "refs/heads/main"];
    let seen = |asked: &[&str]| kept::refs_seen("git version 0", &root, &git_dir, asked);
    let plain = seen(&asked).expect("a plain checkout's refs are vouched for");
    assert!(
        seen(&asked).as_ref() == Some(&plain),
        "the same at two looks"
    );

    assert!(
        seen(&["refs/heads/../../config"]).is_none(),
        "no plain path"
    );
    assert!(seen(&["HEAD"]).is_none(), "no ref under refs/");

    let origin_head = git_dir.join("refs/remotes/origin/HEAD");
    std::fs::write(&origin_head, "ref: refs/remotes/origin/main\n").expect("a symbolic ref");
    let symbolic = seen(&asked).expect("a symbolic ref to a ref is vouched for");
    assert!(symbolic != plain, "another file, another reading");
    let origin_main = git_dir.join("refs/remotes/origin/main");
    std::fs::write(&origin_main, "ref: refs/remotes/origin/next\n").expect("a second hop");
    assert!(seen(&asked).is_none(), "a symbolic ref to a symbolic ref");
    std::fs::write(&origin_main, format!("{head}\n")).expect("the ref again");
    std::fs::remove_file(&origin_head).expect("rm origin/HEAD");
    assert!(seen(&asked).as_ref() == Some(&plain));

    for borrowed in ["objects/info/alternates", "commondir"] {
        let file = git_dir.join(borrowed);
        std::fs::write(&file, "/nowhere\n").expect("a file git reads another store by");
        assert!(seen(&asked).is_none(), "{borrowed}");
        std::fs::remove_file(&file).expect("rm");
        assert!(
            seen(&asked).as_ref() == Some(&plain),
            "{borrowed} gone again"
        );
    }
}
