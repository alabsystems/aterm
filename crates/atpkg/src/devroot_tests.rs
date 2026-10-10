// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The developer root, in scratch homes only: the survey, the move's refusals, the move
//! and its repairs. Nothing here reads or writes the real `$HOME`, and no test asks
//! Spotlight anything (the one measuring path exercised answers before any `mdfind`).

use super::*;

/// A scratch home, canonical (git records resolved paths: `/private/var/…`), with room
/// beside it for what lives outside a home (`beside`).
struct Home {
    _dir: crate::testdir::Dir,
    path: PathBuf,
}

impl Home {
    fn new(label: &str) -> Self {
        let dir = crate::testdir::dir(&format!("atpkg-devroot-{label}-"));
        let path = std::fs::canonicalize(&*dir).unwrap().join("home");
        std::fs::create_dir_all(&path).unwrap();
        Self { _dir: dir, path }
    }

    /// A path in the scratch directory, outside the home.
    fn beside(&self, rel: &str) -> PathBuf {
        self.path.parent().unwrap().join(rel)
    }
}

impl std::ops::Deref for Home {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

/// `git <args>` in `dir`, isolated from the developer's own git configuration (a local
/// submodule URL is allowed), its output.
fn git_run(dir: &Path, args: &[&str]) -> std::process::Output {
    aterm_uds::spawnfd::command("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs")
}

/// [`git_run`], asserting success; its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A committed repository at `dir`.
fn repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("README"), b"x\n").unwrap();
    git(dir, &["add", "README"]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

/// A linked worktree of `main` at `at`, on a branch of its own.
fn worktree(main: &Path, at: &Path, branch: &str) {
    git(
        main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            &at.display().to_string(),
        ],
    );
}

/// A committed repository at `main` with a submodule at `vendor/sub` cloned from
/// `origin` (a repository of its own, made here).
fn repo_with_submodule(main: &Path, origin: &Path) {
    repo(origin);
    repo(main);
    git(
        main,
        &[
            "submodule",
            "add",
            "-q",
            &origin.display().to_string(),
            "vendor/sub",
        ],
    );
    git(main, &["commit", "-q", "-m", "submodule"]);
}

/// A linked worktree of `main` at `at` with its submodules checked out — their git
/// directories in `<main>/.git/worktrees/<id>/modules`, named by RELATIVE `.git` files.
fn worktree_with_submodule(main: &Path, at: &Path, branch: &str) {
    worktree(main, at, branch);
    git(at, &["submodule", "update", "--init", "-q"]);
    let link = std::fs::read_to_string(at.join("vendor/sub/.git")).unwrap();
    assert!(link.starts_with("gitdir: ../"), "relative: {link}");
}

/// A holder scan that sees nothing.
fn nobody(_: &[PathBuf]) -> std::io::Result<aterm_uds::holders::PathScan> {
    Ok(aterm_uds::holders::PathScan::default())
}

fn paths(cs: &[Checkout]) -> Vec<PathBuf> {
    cs.iter().map(|c| c.path.clone()).collect()
}

/// The blockers `plan` holds against `p`.
fn why<'a>(plan: &'a Plan, p: &Path) -> Vec<&'a Blocker> {
    plan.blocked
        .iter()
        .filter(|(q, _)| q == p)
        .map(|(_, b)| b)
        .collect()
}

/// `p` at its place under the root.
fn new_place(home: &Path, p: &Path) -> PathBuf {
    root(home).join(p.strip_prefix(home).unwrap())
}

#[test]
fn the_root_is_developer_under_home() {
    assert_eq!(
        root(Path::new("/Users//u")),
        PathBuf::from("/Users//u/Developer")
    );
}

/// Claude Code's key: every character that is not an ASCII letter or digit is `-`.
#[test]
fn claude_keys_replace_every_non_alphanumeric() {
    assert_eq!(
        claude_key(Path::new("/Users//u/Developer/aterm")),
        "-Users-u-Developer-aterm"
    );
    assert_eq!(
        claude_key(Path::new("/Users//u/aterm-wa.devroot")),
        "-Users-u-aterm-wa-devroot"
    );
    // One `-` per UTF-16 unit, as the JavaScript that writes the key counts them.
    assert_eq!(claude_key(Path::new("/a/\u{1F600}")), "-a---");
}

/// THE SURVEY: checkouts at depth one and two, linked worktrees wherever their
/// repository registers them under `$HOME`, never a dot-folder, `Library` or a
/// consent-guarded folder, never the root, and a worktree nested inside a checkout moves
/// with it rather than on its own.
#[test]
fn the_survey_finds_checkouts_and_worktrees_outside_the_root_and_skips_the_rest() {
    let home = Home::new("survey");
    let a = home.join("a");
    repo(&a);
    let a_wt = home.join("a-wt");
    worktree(&a, &a_wt, "wt");
    let deep = home.join("deep/er/x");
    std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
    worktree(&a, &deep, "deep");
    let nested = a.join(".claude/worktrees/n");
    worktree(&a, &nested, "nested");
    let b = home.join("src/b");
    repo(&b);
    for skipped in [".hidden/c", "Library/d", "Documents/e", "Downloads/f"] {
        std::fs::create_dir_all(home.join(skipped).join(".git")).unwrap();
    }
    let inside = root(&home).join("f");
    repo(&inside);
    // A symlink to a checkout is not a checkout.
    #[cfg(unix)]
    std::os::unix::fs::symlink(&a, home.join("link")).unwrap();
    // Too deep for the walk, and registered by nobody.
    std::fs::create_dir_all(home.join("x/y/z/.git")).unwrap();

    let s = survey(&home);
    assert_eq!(s.root, root(&home));
    assert_eq!(s.root_is, RootIs::Folder);
    assert_eq!(
        paths(&s.outside),
        [a.clone(), a_wt.clone(), deep.clone(), b.clone()],
        "{s:#?}"
    );
    assert_eq!(paths(&s.inside), std::slice::from_ref(&inside));
    let by = |p: &Path| s.outside.iter().find(|c| c.path == p).unwrap().clone();
    assert!(!by(&a).linked);
    assert_eq!(by(&a).common, a.join(".git"));
    assert!(by(&a_wt).linked, "a worktree is linked");
    assert_eq!(by(&a_wt).common, a.join(".git"), "and names its repository");
    assert!(by(&deep).linked);
    assert!(!by(&b).linked);

    let row = devroot_row(&s);
    assert_eq!(row.level, Level::Warn);
    assert!(
        row.text
            .starts_with("4 git checkouts outside ~/Developer (~/a, ~/a-wt, ~/deep/er/x, +1 more)"),
        "{}",
        row.text
    );
    // It says what to do about Spotlight; whether Spotlight skips the root is the
    // measurement's to say.
    assert!(
        row.text
            .contains("the one folder to add to System Settings"),
        "{}",
        row.text
    );
    assert!(!row.text.contains("kept out of"), "{}", row.text);
}

/// An empty home and a home whose checkouts are all inside both read `ok`; a checkout
/// outside with no `~/Developer` yet is a `note` (a Mac that keeps its code elsewhere is
/// not unhealthy), and a `warn` once the folder exists.
#[test]
fn the_devroot_row_is_ok_when_nothing_is_outside_and_a_note_before_the_root() {
    let home = Home::new("row-ok");
    assert_eq!(devroot_row(&survey(&home)).level, Level::Ok);
    repo(&home.join("out"));
    let s = survey(&home);
    assert_eq!(s.root_is, RootIs::Missing);
    assert_eq!(devroot_row(&s).level, Level::Note);
    std::fs::rename(home.join("out"), home.join("one")).unwrap();
    std::fs::create_dir_all(root(&home)).unwrap();
    assert_eq!(devroot_row(&survey(&home)).level, Level::Warn);
    std::fs::rename(home.join("one"), root(&home).join("one")).unwrap();
    let row = devroot_row(&survey(&home));
    assert_eq!(row.level, Level::Ok);
    assert_eq!(row.text, "the one git checkout under ~ is in ~/Developer");
}

/// THE ROOT IS WHAT IT IS, NOT HOW IT IS SPELLED: on a case-insensitive volume an owner's
/// `~/developer` is the root — never walked as a folder of checkouts outside it, and never
/// nested into (`~/developer/developer/proj`). A case-sensitive volume has no such
/// folder to find, and the test says nothing there.
#[test]
fn the_root_is_found_by_what_it_is_not_how_it_is_spelled() {
    let home = Home::new("case");
    std::fs::create_dir_all(home.join("developer")).unwrap();
    if !root(&home).is_dir() {
        return;
    }
    let proj = home.join("developer/proj");
    repo(&proj);
    let out = home.join("out");
    repo(&out);
    let s = survey(&home);
    assert_eq!(s.root, home.join("developer"), "the on-disk spelling");
    assert_eq!(s.root_is, RootIs::Folder);
    assert_eq!(paths(&s.inside), std::slice::from_ref(&proj));
    assert_eq!(paths(&s.outside), std::slice::from_ref(&out));
    let p = plan(&s, &Selection::All, &nobody, &Recorded::default()).unwrap();
    assert_eq!(p.moves.len(), 1);
    assert_eq!(p.moves[0].to, home.join("developer/out"));
    // A worktree registered under yet another spelling of the root is inside it too.
    worktree(&out, &home.join("DEVELOPER/out-wt"), "wt");
    let s = survey(&home);
    assert_eq!(paths(&s.outside), std::slice::from_ref(&out), "{s:#?}");
}

/// A ROOT THAT IS A LINK IS NEVER OPENED: the unattended look must not list, or plant a
/// probe in, a removable volume in aterm's name. The survey reads nothing through it,
/// the Spotlight measurement says why it did not measure, the row is a note naming the
/// link, the move refuses — and the volume's folder is exactly as it was.
#[cfg(unix)]
#[test]
fn a_root_that_is_a_link_is_never_opened() {
    let home = Home::new("link-root");
    let volume = home.beside("Volumes/fast/Developer");
    repo(&volume.join("proj"));
    std::os::unix::fs::symlink(&volume, root(&home)).unwrap();
    let out = home.join("out");
    repo(&out);
    let listing = |dir: &Path| -> Vec<PathBuf> {
        let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        names.sort();
        names
    };
    let before = (listing(&volume), listing(volume.parent().unwrap()));

    let s = survey(&home);
    assert!(
        matches!(&s.root_is, RootIs::NotOpened(why) if why.contains("symbolic link")),
        "{:?}",
        s.root_is
    );
    assert!(s.inside.is_empty(), "nothing is read through the link");
    assert_eq!(paths(&s.outside), std::slice::from_ref(&out));
    let row = devroot_row(&s);
    assert_eq!(row.level, Level::Note);
    assert!(row.text.contains("symbolic link"), "{}", row.text);
    if crate::noindex::SUPPORTED {
        assert!(
            matches!(spotlight(&home), Spotlight::NotMeasured(ref why) if why.contains("symbolic link")),
            "{:?}",
            spotlight(&home)
        );
    }
    assert!(plan(&s, &Selection::All, &nobody, &Recorded::default()).is_err());
    assert_eq!(
        (listing(&volume), listing(volume.parent().unwrap())),
        before,
        "nothing planted"
    );
}

/// THE SPOTLIGHT MEASUREMENT: no root, nothing to measure; a root whose own parent is
/// excluded by name answers `unknown` without planting anything (the probe would be
/// found by a live scan, a false `Indexed`). The rows decide from the verdict alone.
#[test]
fn the_spotlight_measurement_needs_a_root_and_never_guesses() {
    let home = Home::new("spotlight");
    assert_eq!(
        spotlight(&home),
        if crate::noindex::SUPPORTED {
            Spotlight::NoRoot
        } else {
            Spotlight::Measured(Verdict::NotApplicable)
        }
    );
    let row = spotlight_row(&Spotlight::NoRoot).unwrap();
    assert_eq!(row.level, Level::Note);
    if crate::noindex::SUPPORTED {
        let hidden = home.join("box.noindex");
        std::fs::create_dir_all(root(&hidden)).unwrap();
        let got = spotlight(&hidden);
        assert!(
            matches!(
                got,
                Spotlight::Measured(Verdict::Unknown(crate::noindex::Unmeasured::ScopeExcluded(
                    _
                )))
            ),
            "{got:?}"
        );
        assert_eq!(
            std::fs::read_dir(root(&hidden)).unwrap().count(),
            0,
            "nothing planted"
        );
    }
    assert_eq!(
        spotlight_row(&Spotlight::Measured(Verdict::Excluded)).map(|r| r.level),
        Some(Level::Ok)
    );
    assert_eq!(
        spotlight_row(&Spotlight::Measured(Verdict::Indexed)).map(|r| r.level),
        Some(Level::Warn)
    );
    assert_eq!(
        spotlight_row(&Spotlight::Measured(Verdict::NotApplicable)),
        None
    );
}

/// THE MOVE REFUSES, naming each reason, while anything holds a checkout: uncommitted
/// work, a process working in it, an agent session in it, a git lock, a destination in
/// the way, and Claude Code state already at the new key. Nothing is moved by a plan.
#[cfg(unix)]
#[test]
fn the_move_refuses_dirty_busy_agent_locked_and_occupied_checkouts() {
    let home = Home::new("refuse");
    let dirty = home.join("dirty");
    repo(&dirty);
    std::fs::write(dirty.join("new.txt"), b"uncommitted\n").unwrap();
    let busy = home.join("busy");
    repo(&busy);
    let agent = home.join("agent");
    repo(&agent);
    let locked = home.join("locked");
    repo(&locked);
    std::fs::write(locked.join(".git/index.lock"), b"").unwrap();
    let taken = home.join("taken");
    repo(&taken);
    std::fs::create_dir_all(root(&home).join("taken")).unwrap();
    let clash = home.join("clash");
    repo(&clash);
    let projects = home.join(".claude/projects");
    std::fs::create_dir_all(projects.join(claude_key(&clash))).unwrap();
    std::fs::create_dir_all(projects.join(claude_key(&root(&home).join("clash")))).unwrap();
    let free = home.join("free");
    repo(&free);

    // A shell's working directory, and a Claude Code (by its argv[0]) working in another.
    let mut sleeper = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .current_dir(&busy)
        .spawn()
        .unwrap();
    let mut claude = aterm_uds::spawnfd::command("/bin/sleep")
        .arg0("claude")
        .arg("60")
        .current_dir(&agent)
        .spawn()
        .unwrap();
    let pids = [sleeper.id(), claude.id()];
    let holders = |dirs: &[PathBuf]| aterm_uds::holders::scan_path_users_of(&pids, dirs, &[]);

    let s = survey(&home);
    let plan = plan(&s, &Selection::All, &holders, &Recorded::default()).unwrap();
    let _ = sleeper.kill();
    let _ = claude.kill();
    let _ = sleeper.wait();
    let _ = claude.wait();

    assert!(
        matches!(why(&plan, &dirty)[..], [Blocker::Dirty { rows: 1 }]),
        "{:?}",
        why(&plan, &dirty)
    );
    assert!(
        matches!(
            why(&plan, &busy)[..],
            [Blocker::InUse {
                how: "cwd",
                at: None,
                ..
            }]
        ),
        "{:?}",
        why(&plan, &busy)
    );
    assert!(
        matches!(&why(&plan, &agent)[..], [Blocker::Agent { program, at: None, .. }] if program == "claude"),
        "{:?}",
        why(&plan, &agent)
    );
    assert!(
        matches!(why(&plan, &locked)[..], [Blocker::GitLock(_)]),
        "{:?}",
        why(&plan, &locked)
    );
    assert!(
        matches!(why(&plan, &taken)[..], [Blocker::Destination(_)]),
        "{:?}",
        why(&plan, &taken)
    );
    assert!(
        matches!(why(&plan, &clash)[..], [Blocker::AgentStateClash(_)]),
        "{:?}",
        why(&plan, &clash)
    );
    assert!(why(&plan, &free).is_empty(), "{:?}", why(&plan, &free));
    let lines = render_plan(&s, &plan);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("BLOCKED ~/agent -> ~/Developer/agent")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("claude (pid") && l.contains("quit that session first")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("move    ~/free -> ~/Developer/free")),
        "{lines:#?}"
    );
    for dir in [&dirty, &busy, &agent, &locked, &taken, &clash, &free] {
        assert!(dir.is_dir(), "a plan moves nothing: {}", dir.display());
    }

    // A named directory must be one the survey found outside the root.
    let none = Recorded::default();
    assert!(
        crate::devroot::plan(
            &s,
            &Selection::Named(vec![home.join("nowhere")]),
            &nobody,
            &none
        )
        .is_err()
    );
    let named =
        crate::devroot::plan(&s, &Selection::Named(vec![free.clone()]), &nobody, &none).unwrap();
    assert_eq!(named.moves.len(), 1);
    assert!(named.blocked.is_empty(), "{:?}", named.blocked);
}

/// A PROCESS IN ANOTHER CHECKOUT OF THE REPOSITORY HOLDS IT: a worktree outside the home
/// (an agent's `/private/tmp/claude-*` worktree) shares the git directory the move
/// renames, so a session working there refuses the move as surely as one in the
/// checkout itself — and the refusal says where it works.
#[cfg(unix)]
#[test]
fn a_process_in_another_checkout_of_the_repository_holds_it() {
    let home = Home::new("wide-scan");
    let main = home.join("app");
    repo(&main);
    let far = home.beside("tmp/claude-wt");
    std::fs::create_dir_all(far.parent().unwrap()).unwrap();
    worktree(&main, &far, "far");
    let mut sleeper = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .current_dir(&far)
        .spawn()
        .unwrap();
    let pids = [sleeper.id()];
    let holders = |dirs: &[PathBuf]| aterm_uds::holders::scan_path_users_of(&pids, dirs, &[]);
    let s = survey(&home);
    let plan = plan(&s, &Selection::All, &holders, &Recorded::default()).unwrap();
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert!(
        matches!(&why(&plan, &main)[..], [Blocker::InUse { how: "cwd", at: Some(at), .. }] if *at == far),
        "{:?}",
        plan.blocked
    );
    let lines = render_plan(&s, &plan);
    assert!(
        lines.iter().any(|l| l.contains(&far.display().to_string())
            && l.contains("shares this repository's git directory")),
        "{lines:#?}"
    );
}

/// A REPOSITORY WITH SUBMODULES MOVES WHOLE. Its submodules' `.git` files and
/// `core.worktree` are relative, and `git worktree repair` does not touch them, so one
/// checkout moved without the rest loses its submodule. The plan refuses each split,
/// naming the checkout that would stay: one a named selection left out, and one outside
/// the home that cannot move at all. Nothing moves.
#[test]
fn a_repository_with_submodules_is_never_split() {
    let home = Home::new("split");
    let main = home.join("app");
    repo_with_submodule(&main, &home.beside("origin"));
    let wt = home.join("app-wt");
    worktree_with_submodule(&main, &wt, "wt");

    let s = survey(&home);
    let named = plan(
        &s,
        &Selection::Named(vec![main.clone()]),
        &nobody,
        &Recorded::default(),
    )
    .unwrap();
    assert!(
        matches!(
            &why(&named, &main)[..],
            [Blocker::Split { checkout, main: false, apart: Apart::NotNamed }] if *checkout == wt
        ),
        "{:?}",
        named.blocked
    );
    let said = render_plan(&s, &named).join("\n");
    assert!(said.contains("name it too, or use --all"), "{said}");
    // Named whole, it moves whole.
    let both = plan(
        &s,
        &Selection::Named(vec![main.clone(), wt.clone()]),
        &nobody,
        &Recorded::default(),
    )
    .unwrap();
    assert!(both.blocked.is_empty(), "{:?}", both.blocked);

    // A worktree outside the home cannot move with it: --all refuses the repository.
    let far = home.beside("tmp/claude-wt");
    std::fs::create_dir_all(far.parent().unwrap()).unwrap();
    worktree_with_submodule(&main, &far, "far");
    let s = survey(&home);
    let all = plan(&s, &Selection::All, &nobody, &Recorded::default()).unwrap();
    assert!(
        why(&all, &main).iter().any(|b| matches!(
            b,
            Blocker::Split { checkout, apart: Apart::OutsideHome, .. } if *checkout == far
        )),
        "{:?}",
        all.blocked
    );
    for dir in [&main, &wt, &far] {
        assert!(dir.is_dir(), "a plan moves nothing: {}", dir.display());
    }
}

/// MOVED WHOLE, A REPOSITORY WITH SUBMODULES IS WHOLE AFTER: the main checkout and its
/// worktree move together, every relative submodule link still resolves, and the
/// absolute paths in submodule git directories are rewritten — wherever that git
/// directory lives (the gate snapshot keeps its own in `.aterm-verify/modules`, inside the
/// worktree). The run's own check says each checkout answers `git status`.
#[test]
fn a_repository_with_submodules_moved_whole_is_whole() {
    let home = Home::new("whole");
    let main = home.join("app");
    let origin = home.beside("origin");
    repo_with_submodule(&main, &origin);
    std::fs::write(main.join(".git/info/exclude"), b".aterm-verify/\n").unwrap();
    let wt = home.join("app-wt");
    worktree_with_submodule(&main, &wt, "wt");
    // The gate snapshot's shape: the worktree's submodule git directory moved into the
    // worktree itself, named relatively, borrowing objects through an ABSOLUTE alternate
    // into the main checkout's own submodule git directory.
    let admin = git(&wt, &["rev-parse", "--git-dir"]);
    let in_admin = PathBuf::from(admin.trim()).join("modules/vendor/sub");
    let own = wt.join(".aterm-verify/modules/vendor/sub");
    std::fs::create_dir_all(own.parent().unwrap()).unwrap();
    std::fs::rename(&in_admin, &own).unwrap();
    std::fs::write(
        wt.join("vendor/sub/.git"),
        b"gitdir: ../../.aterm-verify/modules/vendor/sub\n",
    )
    .unwrap();
    git(
        &wt,
        &[
            "config",
            "--file",
            &own.join("config").display().to_string(),
            "core.worktree",
            "../../../../vendor/sub",
        ],
    );
    let borrowed = main.join(".git/modules/vendor/sub/objects");
    let untouched = origin.join(".git/objects");
    std::fs::write(
        own.join("objects/info/alternates"),
        format!("{}\n{}\n", borrowed.display(), untouched.display()),
    )
    .unwrap();
    assert!(
        git_run(&wt, &["status", "--porcelain"]).status.success(),
        "the fixture is whole before the move"
    );

    let s = survey(&home);
    assert_eq!(paths(&s.outside), [main.clone(), wt.clone()]);
    let plan = plan(&s, &Selection::All, &nobody, &Recorded::default()).unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    let report = execute(&s, &plan, None);
    assert!(report.failed.is_empty(), "{report:#?}");
    assert!(
        report
            .done
            .iter()
            .any(|l| l.starts_with("checked ") && l.contains("each answers")),
        "{report:#?}"
    );
    let (main_now, wt_now) = (new_place(&home, &main), new_place(&home, &wt));
    for dir in [&main_now, &wt_now] {
        let out = git_run(dir, &["status", "--porcelain"]);
        assert!(
            out.status.success(),
            "{}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            git_run(&dir.join("vendor/sub"), &["status", "--porcelain"])
                .status
                .success(),
            "the submodule of {} resolves",
            dir.display()
        );
    }
    assert_eq!(
        std::fs::read_to_string(new_place(&home, &own).join("objects/info/alternates")).unwrap(),
        format!(
            "{}\n{}\n",
            new_place(&home, &borrowed).display(),
            untouched.display()
        ),
        "the moved alternate follows; the one outside stays"
    );
}

/// THE CHECK IS REAL: handed a plan that splits a repository with submodules (the plan
/// would refuse it; here it is made by hand), the run moves what it was told and then
/// says, as a FAILURE naming the worktree, that it no longer answers `git status` —
/// never "done".
#[test]
fn the_run_names_a_checkout_it_left_broken() {
    let home = Home::new("check");
    let main = home.join("app");
    repo_with_submodule(&main, &home.beside("origin"));
    let wt = home.join("app-wt");
    worktree_with_submodule(&main, &wt, "wt");
    let s = survey(&home);
    let split = Plan {
        moves: vec![Move {
            from: main.clone(),
            to: new_place(&home, &main),
            common: main.join(".git"),
        }],
        ..Plan::default()
    };
    let report = execute(&s, &split, None);
    assert!(
        report
            .failed
            .iter()
            .any(|l| l.starts_with("~/app-wt is not whole after the move")),
        "{report:#?}"
    );
}

/// A REGISTRATION THAT WAS STALE BEFORE THE MOVE IS NOT THE MOVE'S FAILURE: a worktree
/// deleted from `/tmp` long ago leaves its registration behind (`prunable`); the move of
/// its repository succeeds and says so.
#[test]
fn a_stale_registration_is_not_the_moves_failure() {
    let home = Home::new("stale");
    let main = home.join("app");
    repo(&main);
    let gone = home.beside("gone");
    worktree(&main, &gone, "gone");
    std::fs::remove_dir_all(&gone).unwrap();
    let s = survey(&home);
    let plan = plan(&s, &Selection::All, &nobody, &Recorded::default()).unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    let report = execute(&s, &plan, None);
    assert!(report.failed.is_empty(), "{report:#?}");
    assert!(new_place(&home, &main).join(".git").is_dir());
}

/// A RENAME THAT CANNOT HAPPEN IS REFUSED BEFORE ANYTHING MOVES: a destination folder
/// that cannot be written holds every move, named.
#[cfg(unix)]
#[test]
fn a_destination_that_cannot_be_written_holds_the_move() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = Home::new("unwritable");
    let app = home.join("app");
    repo(&app);
    std::fs::create_dir_all(root(&home)).unwrap();
    std::fs::set_permissions(root(&home), std::fs::Permissions::from_mode(0o555)).unwrap();
    let s = survey(&home);
    let plan = plan(&s, &Selection::All, &nobody, &Recorded::default());
    std::fs::set_permissions(root(&home), std::fs::Permissions::from_mode(0o755)).unwrap();
    let plan = plan.unwrap();
    assert!(
        matches!(&why(&plan, &app)[..], [Blocker::Unmovable(why)] if why.contains("cannot be written")),
        "{:?}",
        plan.blocked
    );
    assert!(app.is_dir());
}

/// AN INTERRUPT WAITS FOR THE MOVE: a SIGHUP (a closed tab) arriving while the run holds
/// the interrupts is recorded and handed back when it ends; the process lives on to say
/// so, and nothing is held once the run is over.
#[cfg(unix)]
#[test]
fn an_interrupt_during_the_move_waits_for_it_to_end() {
    let (value, held) = signals::holding(|| {
        // SAFETY: raising a signal this run's handler records.
        unsafe { libc::raise(libc::SIGHUP) };
        7
    });
    assert_eq!(value, 7);
    assert_eq!(held, Some(libc::SIGHUP));
    assert_eq!(signals::holding(|| ()).1, None);
}

/// THE MOVE AND ITS REPAIRS, end to end in a scratch home: a main checkout with a linked
/// worktree beside it and one nested inside it, a sibling clone whose git config names the
/// main (the cut clone's `aterm-release.receiptsFrom`), Claude Code's path-keyed
/// directories for the checkout and for a subdirectory of it, the publication engine's
/// registry rows, and an `aterm pkg link` dev link into the checkout. After it: every
/// worktree link resolves both ways, every recorded path names the new place, the dev
/// link's shim and marker name it too, the transcripts are byte-for-byte what they were,
/// and a second run finds nothing to do. The `aterm.toml` row naming the old place is the
/// owner's file: the plan says what to change in it.
#[test]
fn the_move_repairs_worktree_links_recorded_paths_and_agent_dirs() {
    let home = Home::new("move");
    let main = home.join("app");
    repo(&main);
    std::fs::create_dir_all(main.join("crates/core")).unwrap();
    let wt = home.join("app-wt");
    worktree(&main, &wt, "wt");
    // Claude Code's own worktrees live under `.claude/`, and the dev link's build output
    // under `target/`, both of which the checkout ignores.
    std::fs::write(main.join(".git/info/exclude"), b".claude/\ntarget/\n").unwrap();
    let nested = main.join(".claude/worktrees/n");
    worktree(&main, &nested, "nested");
    let bin = main.join("target/release/devtool");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let store = crate::testdir::layout("atpkg-devroot-store-");
    crate::linkmode::link(
        &store,
        "devtool",
        &main,
        &[PathBuf::from("target/release/devtool")],
    )
    .unwrap();
    let cut = home.join("cut");
    repo(&cut);
    git(
        &cut,
        &[
            "config",
            "--local",
            "--add",
            "aterm-release.receiptsFrom",
            &main.display().to_string(),
        ],
    );
    git(
        &cut,
        &[
            "config",
            "--local",
            "--add",
            "aterm-release.receiptsFrom",
            "/elsewhere",
        ],
    );
    let engine = home.join("publication");
    repo(&engine);
    std::fs::create_dir_all(engine.join("registry")).unwrap();
    let registry = "[[repo]]\nname = \"app\"\npath = \"~/app\"\n\n[[repo]]\nname = \"far\"\n\
                    path = \"~/elsewhere\"\n";
    std::fs::write(engine.join("registry/repos.toml"), registry).unwrap();
    git(&engine, &["add", "registry/repos.toml"]);
    git(&engine, &["commit", "-q", "-m", "registry"]);
    let projects = home.join(".claude/projects");
    let transcript = b"{\"cwd\":\"/old/path/stays\"}\n";
    for dir in [&main, &main.join("crates/core")] {
        let key = projects.join(claude_key(dir));
        std::fs::create_dir_all(&key).unwrap();
        std::fs::write(key.join("s.jsonl"), transcript).unwrap();
    }
    // Another checkout's key that only LOOKS like a subdirectory of `app` (`app-wt`).
    std::fs::create_dir_all(projects.join(claude_key(&wt))).unwrap();

    let s = survey(&home);
    assert_eq!(
        paths(&s.outside),
        [main.clone(), wt.clone(), cut.clone(), engine.clone()]
    );
    let recorded = Recorded {
        dev_links: vec![("devtool".to_string(), main.clone())],
        config_links: vec![("devtool".to_string(), "~/app".to_string(), main.clone())],
    };
    let plan = plan(&s, &Selection::All, &nobody, &recorded).unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    let new = |p: &Path| new_place(&home, p);
    assert_eq!(
        plan.relinks,
        [Relink {
            program: "devtool".to_string(),
            from: main.clone(),
            to: new(&main),
        }]
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("[packages.links] devtool = \"~/app\"")
                && n.contains("\"~/Developer/app\"")),
        "{:?}",
        plan.notes
    );
    let report = execute(&s, &plan, Some(&store));
    assert!(report.failed.is_empty(), "{report:#?}");
    assert_eq!(report.held_signal, None);

    for p in [&main, &wt, &cut, &engine] {
        assert!(!p.exists() && new(p).is_dir(), "{} moved", p.display());
    }
    // Worktree links, both directions, for the one beside and the one inside.
    let list = git(&new(&main), &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("prunable"), "{list}");
    assert!(
        list.contains(&format!("worktree {}", new(&wt).display())),
        "{list}"
    );
    assert!(
        list.contains(&format!("worktree {}", new(&nested).display())),
        "{list}"
    );
    assert_eq!(
        git(&new(&wt), &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "wt"
    );
    assert_eq!(
        git(&new(&nested), &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "nested"
    );
    // The receipt store a cut clone names, and nothing else it names.
    let from = git(
        &new(&cut),
        &[
            "config",
            "--local",
            "--get-all",
            "aterm-release.receiptsFrom",
        ],
    );
    assert_eq!(
        from.lines().collect::<Vec<_>>(),
        [new(&main).display().to_string().as_str(), "/elsewhere"]
    );
    // The engine's registry: the moved row, and only that row.
    assert_eq!(
        std::fs::read_to_string(new(&engine).join("registry/repos.toml")).unwrap(),
        registry.replace("path = \"~/app\"", "path = \"~/Developer/app\"")
    );
    // The dev link: its marker and its shim both name the new place.
    assert_eq!(
        crate::linkmode::linked_checkout(&store, "devtool"),
        Some(new(&main))
    );
    let shim = store.shim(&crate::store::ToolName::new("devtool").unwrap());
    let target = crate::platform::resolve_shim(&shim).expect("the shim forwards");
    assert!(target.starts_with(new(&main)), "{}", target.display());
    // Claude Code's keys: the checkout's and its subdirectory's, contents untouched; the
    // look-alike key is the other checkout's (renamed for `app-wt`'s own move).
    for dir in [&main, &main.join("crates/core")] {
        assert!(!projects.join(claude_key(dir)).exists());
        let moved = projects.join(claude_key(&new(dir))).join("s.jsonl");
        assert_eq!(
            std::fs::read(&moved).unwrap(),
            transcript,
            "{}",
            moved.display()
        );
    }
    assert!(projects.join(claude_key(&new(&wt))).is_dir());

    // Idempotent: nothing is outside any more.
    let again = survey(&home);
    assert!(again.outside.is_empty(), "{again:#?}");
    assert_eq!(devroot_row(&again).level, Level::Ok);
    let none = crate::devroot::plan(&again, &Selection::All, &nobody, &recorded).unwrap();
    assert!(none.moves.is_empty() && none.blocked.is_empty());
}

/// The engine is found where it is: under the root once moved, the pre-root
/// `~/publication` before, and the root's place when neither exists.
#[test]
fn the_publication_engine_is_found_where_it_is() {
    let home = Home::new("engine");
    let h: &Path = &home;
    assert_eq!(
        publication_engine(h),
        h.join("Developer").join("publication")
    );
    std::fs::create_dir_all(h.join("publication")).unwrap();
    assert_eq!(publication_engine(h), h.join("publication"));
    std::fs::create_dir_all(h.join("Developer").join("publication")).unwrap();
    assert_eq!(
        publication_engine(h),
        h.join("Developer").join("publication")
    );
}
