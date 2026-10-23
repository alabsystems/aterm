// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows tier-3 disk-idle tests: `.cmd` shims, path canonicalization, 8.3 short paths, and per-drive volume walks.
use super::*;
use crate::harness::disk::tests::{Tmp, lay_target, now_after_write};
use aterm_sysprobe::volumes::{Mount, NETWORK};

const DAY: i64 = 86_400;

/// A checkout at `<tmp>\repo`: `.git\index`, `.git\logs\HEAD`, a manifest
/// and a source file, and its build directory `target\` (one profile).
fn checkout(tmp: &Tmp) -> (PathBuf, PathBuf) {
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git").join("logs")).expect("git");
    std::fs::write(repo.join(".git").join("index"), b"DIRC").expect("index");
    std::fs::write(repo.join(".git").join("logs").join("HEAD"), b"0 1 a\n").expect("head");
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::write(repo.join("Cargo.toml"), b"[package]\n").expect("manifest");
    std::fs::write(repo.join("src").join("main.rs"), b"fn main() {}\n").expect("source");
    let target = lay_target(&repo.join("target"));
    let canon = |p: PathBuf| std::fs::canonicalize(p).expect("canonical");
    (canon(repo), canon(target))
}

/// `create_dir_all`, answering the path.
fn mkdirs(p: PathBuf) -> PathBuf {
    std::fs::create_dir_all(&p).expect("mkdir");
    p
}

/// `p` as a person types it: plain (no `\\?\`), every letter in capitals.
fn shouted(p: &Path) -> PathBuf {
    PathBuf::from(
        aterm_sysprobe::paths::plain(p)
            .to_string_lossy()
            .to_uppercase(),
    )
}

/// `C:\` and `D:\`, two local volumes, and the network: a mount table no
/// machine need have.
fn two_drives() -> Table {
    let volume = |on: &str, n: u32, guid: &str| Mount {
        on: PathBuf::from(on),
        from: format!(r"\Device\HarddiskVolume{n}"),
        fstype: String::new(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: Some(format!("Volume{{{guid}}}")),
    };
    Table {
        mounts: vec![
            Mount {
                on: PathBuf::from(NETWORK),
                from: "UNC".to_owned(),
                fstype: String::new(),
                local: false,
                automounted: false,
                removable: false,
                read_only: false,
                container: None,
            },
            volume(r"C:\", 3, "6140fc3b-044f-43d7-b060-cb98170c7a63"),
            volume(r"D:\", 4, "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d"),
        ],
        complete: true,
        firmlinks: Vec::new(),
    }
}

/// A SESSION'S WORKING DIRECTORY IS PLACED BY THE ONE RULE: resolved, in
/// any spelling against a candidate's verbatim one; not resolved (gone, or
/// unreadable), on Windows it keeps every candidate of its volume — and of
/// every volume when it names none (a share, no table).
#[test]
fn a_session_cwd_is_placed_by_the_one_rule_and_an_unresolved_one_by_its_volume() {
    let t = two_drives();
    let repo = Path::new(r"\\?\C:\Work\Repo");
    let placed = |p: &str| SessionCwd::Placed(PathBuf::from(p));
    let unresolved = |p: &str| SessionCwd::Unresolved(PathBuf::from(p));
    assert!(placed(r"c:\WORK\repo\src\").may_work_in(repo, Some(&t)));
    assert!(placed(r"C:\Work\Repo").may_work_in(repo, Some(&t)));
    assert!(placed(r"C:\PROGRA~1\x").may_work_in(repo, Some(&t)));
    assert!(!placed(r"C:\Work\Other").may_work_in(repo, Some(&t)));
    assert!(unresolved(r"C:\gone").may_work_in(repo, Some(&t)));
    assert!(unresolved(r"\\srv\share\x").may_work_in(repo, Some(&t)));
    assert!(unresolved(r"D:\gone").may_work_in(repo, None));
    assert!(!unresolved(r"D:\gone").may_work_in(repo, Some(&t)));
    let real = Tmp::new("idle-cwd-of");
    assert_eq!(
        SessionCwd::of(real.path()),
        SessionCwd::Placed(std::fs::canonicalize(real.path()).expect("canonical"))
    );
    let gone = real.path().join("gone");
    assert_eq!(SessionCwd::of(&gone), SessionCwd::Unresolved(gone.clone()));
    assert_eq!(SessionCwd::of(&gone).path(), gone.as_path());
}

/// TIER 3 ON A WINDOWS CHECKOUT, CLAUSE BY CLAUSE: every clause clear, the
/// build directory is a candidate (the NEGATIVE CONTROL of each refusal
/// below); then each unmet in a Windows spelling keeps it — a session
/// whose working directory is UNKNOWN (the reason named), a session working
/// in it spelled plain and in capitals, one whose folder is gone from its
/// volume, an installed link resolving into it spelled so, and a build
/// directory nested in it spelled so.
#[test]
fn tier_3_on_a_windows_checkout_holds_every_spelling_and_an_unknown_cwd() {
    let tmp = Tmp::new("idle-win-assess");
    let (repo, target) = checkout(&tmp);
    let beside = std::fs::canonicalize(mkdirs(tmp.path().join("beside"))).expect("canonical");
    let now = now_after_write(8);
    let mut t = super::super::scan_target(&target, now, 7, true).expect("a build directory");
    t.origin = Origin::Checkout { root: repo.clone() };
    t.device = aterm_sysprobe::volumes::file_id(&target, true).map(|id| id.dev);
    t.walk_cut_short = None;
    if t.profiles.is_empty() {
        t.profiles.push(super::super::target::Profile {
            path: target.join("debug"),
            last_used: Some(now_after_write(0)),
            incremental: true,
            in_use: None,
            bytes: 8192,
            bytes_partial: false,
        });
    }
    let table = aterm_sysprobe::volumes::table();
    let found = [target.clone()];
    let none: Result<Vec<PathBuf>, String> = Ok(Vec::new());
    let cx = Context {
        now,
        window_s: 7 * DAY,
        device: t.device,
        session_cwds: &[],
        cwd_unknown: None,
        links: &none,
        found: &found,
        home: None,
        mounts: Some(&table),
    };
    let fences = |_: &TargetDir| Ok(());
    let clear = assess(&t, &cx, &fences);
    assert!(clear.is_ok(), "{clear:?}");
    assert_eq!(
        clear.map(|f| aterm_sysprobe::paths::windows_key(&f.checkout)),
        Ok(aterm_sysprobe::paths::windows_key(&repo))
    );

    let why = "tab 2 reported no working directory this window can place, so any checkout \
               may be one's";
    let unknown = Context {
        cwd_unknown: Some(why),
        ..cx
    };
    assert_eq!(assess(&t, &unknown, &fences), Err(why.to_owned()));

    let cwds = |cwd: SessionCwd| {
        assess(
            &t,
            &Context {
                session_cwds: &[cwd],
                ..cx
            },
            &fences,
        )
    };
    for held in [
        SessionCwd::of(&shouted(&repo.join("src"))),
        SessionCwd::of(&shouted(&target)),
        SessionCwd::of(&tmp.path().join("gone")),
    ] {
        let kept = cwds(held.clone());
        assert!(
            kept.as_ref()
                .is_err_and(|w| w.starts_with("a session works in")),
            "{held:?}: {kept:?}"
        );
    }
    let free = cwds(SessionCwd::of(&beside));
    assert!(free.is_ok(), "{free:?}");

    let linked: Result<Vec<PathBuf>, String> =
        Ok(vec![shouted(&target).join("DEBUG").join("tool.run")]);
    let kept = assess(
        &t,
        &Context {
            links: &linked,
            ..cx
        },
        &fences,
    );
    assert!(
        kept.as_ref()
            .is_err_and(|w| w.starts_with("an installed link")),
        "{kept:?}"
    );
    let nested = [
        target.clone(),
        shouted(&target).join("DEBUG").join("NESTED"),
    ];
    let kept = assess(
        &t,
        &Context {
            found: &nested,
            ..cx
        },
        &fences,
    );
    assert_eq!(
        kept,
        Err("a build directory is inside it, or it is inside one".to_owned())
    );
}

/// CLAUSE 4B PLACES THE KERNEL'S SPELLING: a process whose working directory
/// the kernel writes plain, in capitals, with a trailing separator, uses the
/// candidate canonical as `\\?\C:\…`, and keeps it.
#[test]
fn clause_4b_places_the_kernels_spelling_on_its_candidate() {
    use aterm_uds::holders::{PathScan, PathUse};
    let tmp = Tmp::new("idle-win-4b");
    let (repo, target) = checkout(&tmp);
    let table = aterm_sysprobe::volumes::table();
    let at = |path: PathBuf| {
        move |_: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
            Ok(PathScan {
                users: vec![PathUse {
                    pid: 42,
                    how: "cwd",
                    path: path.clone(),
                }],
                ..PathScan::default()
            })
        }
    };
    let mut kernel = shouted(&repo.join("src")).into_os_string();
    kernel.push("\\");
    let held = processes_clear(
        &[(repo.clone(), target.clone())],
        &at(PathBuf::from(kernel)),
        Some(&table),
    );
    assert!(
        held[0]
            .as_ref()
            .is_err_and(|w| w.starts_with("process 42 uses")),
        "{held:?}"
    );
    let beside = processes_clear(
        &[(repo, target)],
        &at(tmp.path().join("beside")),
        Some(&table),
    );
    assert_eq!(beside, [Ok(())]);
}

/// `path` as `cmd` and `CreateProcessW` take a working directory: the
/// canonical spelling without its verbatim prefix (test input only, never an
/// operation's path).
fn for_cmd(path: &Path) -> PathBuf {
    let s = path.to_str().expect("a test path is UTF-8");
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(s))
}

/// A `cmd.exe` of this test's started in `cwd` exactly as spelled, blocked
/// reading its stdin until dropped: the drop closes the stdin, so it reads
/// end-of-file and exits, and waits for that exit.
struct Waiting(aterm_uds::spawnfd::Child);

impl Waiting {
    fn start(cwd: &Path) -> Waiting {
        let child = aterm_uds::spawnfd::command("cmd")
            .args(["/D", "/Q", "/C", "set /p line="])
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("cmd starts");
        Waiting(child)
    }

    fn id(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

/// CLAUSE 4B KEEPS ONLY THE CANDIDATE AN 8.3-SPELLED WORKING DIRECTORY IS IN:
/// one scan of the running processes over two
/// candidates, and a `cmd.exe` of this test's started in the first one's
/// `target\debug` spelled with 8.3 short names (a process keeps the spelling
/// it was started in as its working directory).
#[test]
fn clause_4b_keeps_only_the_candidate_a_short_spelled_working_directory_is_in() {
    let tmp = Tmp::new("idle-win-4b-short");
    let canon = |p: PathBuf| std::fs::canonicalize(mkdirs(p)).expect("canonical");
    let first = canon(tmp.path().join("a checkout with a long name"));
    let first_target = canon(first.join("target"));
    let debug = canon(first_target.join("debug"));
    let second = canon(tmp.path().join("another checkout"));
    let second_target = canon(second.join("target"));
    let short = aterm_sysprobe::short_spelling(&debug).unwrap_or_else(|| {
        panic!(
            "PREMISE: the volume generates 8.3 short names; none for {}",
            debug.display()
        )
    });
    let table = aterm_sysprobe::volumes::table();
    let candidates = [
        (first.clone(), first_target.clone()),
        (second.clone(), second_target.clone()),
    ];
    for (spelling, short_spelled) in [(for_cmd(&short), true), (for_cmd(&debug), false)] {
        let child = Waiting::start(&spelling);
        let pid = child.id();
        let ours = |dirs: &[PathBuf], skip: &[u32]| {
            aterm_uds::holders::scan_path_users_of_within(
                &[pid],
                dirs,
                skip,
                aterm_spec::wait::MUST_HAPPEN,
            )
        };
        let clear = processes_clear(&candidates, &ours, Some(&table));
        let reported = aterm_sysprobe::process_cwd(pid).expect("its working directory");
        drop(child);
        assert_eq!(
            aterm_sysprobe::paths::short_spelled(&reported).is_some(),
            short_spelled,
            "PREMISE: the child reports the spelling it was started in: {reported:?}"
        );
        assert_eq!(clear.len(), 2, "{clear:?}");
        assert!(
            clear[0]
                .as_ref()
                .is_err_and(|w| w.starts_with(&format!("process {pid} uses"))),
            "{spelling:?}: {clear:?}"
        );
        assert_eq!(clear[1], Ok(()), "{spelling:?}");
    }
}

/// A SESSION'S 8.3-SPELLED WORKING DIRECTORY IS OPENED ONLY ON A LOCAL
/// VOLUME ([`SessionCwd::on`]): a folder a session reported short
/// (`APROJE~1`, as a short `%TEMP%` hands to every child) is spelled long,
/// folder by folder, before the mount table places it — so on the table as
/// read it resolves where its long spelling does, and where the table calls
/// its volume remote it is never opened: `Unresolved`, as the long spelling
/// is.
#[test]
fn a_short_spelled_session_cwd_is_opened_only_on_a_local_volume() {
    let tmp = Tmp::new("idle-win-cwd-short");
    let long = std::fs::canonicalize(mkdirs(tmp.path().join("a project with a long name")))
        .expect("canonical");
    let short = aterm_sysprobe::short_spelling(&long).unwrap_or_else(|| {
        panic!(
            "PREMISE: the volume generates 8.3 short names; none for {}",
            long.display()
        )
    });
    let key = aterm_sysprobe::paths::windows_key;
    assert_eq!(key(&short), None, "PREMISE: the key refuses {short:?}");
    let table = aterm_sysprobe::volumes::table();
    for spelling in [&short, &long] {
        match SessionCwd::on(spelling, Some(&table)) {
            SessionCwd::Placed(p) => assert_eq!(key(&p), key(&long), "{spelling:?}: {p:?}"),
            other @ SessionCwd::Unresolved(_) => panic!("{spelling:?}: {other:?}"),
        }
    }
    let on = aterm_sysprobe::volumes::mount_of(&long, &table)
        .expect("a mount holds it")
        .on
        .clone();
    let mut remote = table.clone();
    for m in remote.mounts.iter_mut().filter(|m| m.on == on) {
        m.local = false;
    }
    for spelling in [&short, &long] {
        assert_eq!(
            SessionCwd::on(spelling, Some(&remote)),
            SessionCwd::Unresolved(spelling.clone()),
            "{spelling:?}"
        );
    }
    let share = PathBuf::from(r"\\nas.invalid\share\PROJEC~1\x");
    assert_eq!(
        SessionCwd::on(&share, Some(&two_drives())),
        SessionCwd::Unresolved(share.clone())
    );
    let long_share = Path::new(r"\\nas.invalid\share\Project\x");
    let walked = resolve_local_reading(long_share, &two_drives());
    assert!(matches!(walked, Ok(None)), "{walked:?}");
}

/// A WALK ENDS ONLY ON A LOCAL FOLDER ([`resolve_local`]): a share's root,
/// or the root of a drive the table calls remote, holds no component for a
/// step to place, and resolves to nothing — as a link onto one does, which
/// ends the walk there (the old walk answered the root it ended on).
#[test]
fn a_walk_that_ends_on_a_remote_root_resolves_to_nothing() {
    let t = two_drives();
    assert_eq!(resolve_local(Path::new(r"\\srv\share\"), &t), None);
    assert_eq!(resolve_local(Path::new(r"\\?\UNC\srv\share\"), &t), None);
    let mut remote = t.clone();
    for m in remote
        .mounts
        .iter_mut()
        .filter(|m| m.on == Path::new(r"D:\"))
    {
        m.local = false;
    }
    assert_eq!(resolve_local(Path::new(r"D:\"), &remote), None);
    assert_eq!(
        resolve_local(Path::new(r"D:\"), &t),
        Some(PathBuf::from(r"D:\"))
    );
}

/// THE WALKS START AT THE PATH'S OWN DRIVE: a path resolves from its drive's
/// root (never `/`), through a junction to the folder it names.
#[test]
fn the_walks_start_at_the_paths_own_drive() {
    let tmp = Tmp::new("idle-win-walks");
    let real = mkdirs(tmp.path().join("real"));
    let links = mkdirs(tmp.path().join("links"));
    atpkg::platform::atomic_symlink(&real, &links.join("tool")).expect("a junction");
    let table = aterm_sysprobe::volumes::table();
    let key = |p: &Path| aterm_sysprobe::paths::windows_key(p);
    let canonical = std::fs::canonicalize(&real).expect("canonical");
    let through = resolve_local(&links.join("tool"), &table).expect("resolved");
    assert_eq!(key(&through), key(&canonical), "{through:?}");
    let direct = resolve_local(&real, &table).expect("resolved");
    assert_eq!(key(&direct), key(&canonical), "{direct:?}");
    assert_eq!(resolve_local(&real.join("missing"), &table), None);
    assert_eq!(resolve_local(Path::new("real"), &table), None);
    assert_eq!(resolve_local(Path::new("/c/Users"), &table), None);
    let cut = Table {
        complete: false,
        ..table.clone()
    };
    assert_eq!(resolve_local(&real, &cut), None);

    let (repo, target) = checkout(&tmp);
    let quiet = tree_quiet(
        &repo,
        std::slice::from_ref(&target),
        now_after_write(8),
        7 * DAY,
        TREE_WALL,
    );
    assert_eq!(quiet, Ok(()));
    let fresh = tree_quiet(&repo, &[target], now_after_write(0), 7 * DAY, TREE_WALL);
    assert!(
        fresh.as_ref().is_err_and(|w| w.contains("was written")),
        "{fresh:?}"
    );
}

/// A WORKTREE'S GIT DIRECTORY IS PLACED ON ITS DRIVE AND READ: a `.git` file
/// naming an absolute git directory (as `git worktree add` writes it) is
/// placed through the mount table and its `index` dated.
#[test]
fn a_worktrees_git_directory_is_placed_on_its_drive_and_read() {
    let tmp = Tmp::new("idle-win-gitdir");
    let gitdir = mkdirs(tmp.path().join("gitdirs").join("wt"));
    std::fs::write(gitdir.join("index"), b"DIRC").expect("index");
    let wt = mkdirs(tmp.path().join("wt"));
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", gitdir.display()).as_bytes(),
    )
    .expect(".git");
    let table = aterm_sysprobe::volumes::table();
    assert_eq!(
        git_quiet(&wt, now_after_write(8), 7 * DAY, Some(&table)),
        Ok(())
    );
    let fresh = git_quiet(&wt, now_after_write(0), 7 * DAY, Some(&table));
    assert!(
        fresh.as_ref().is_err_and(|w| w.contains("its git index")),
        "{fresh:?}"
    );
    let unplaced = git_quiet(&wt, now_after_write(8), 7 * DAY, None);
    assert!(
        unplaced
            .as_ref()
            .is_err_and(|w| w.contains("could not be placed")),
        "{unplaced:?}"
    );
}

/// The kind of the entry at `path`, as the clause's listing reads it.
fn kind(path: &Path) -> std::fs::FileType {
    std::fs::symlink_metadata(path).expect("listed").file_type()
}

/// A SHIM IS A LINK TO WHAT IT RUNS: a forwarding shim atpkg laid reads back
/// as its program, whatever the case of its `.cmd`; NEGATIVE CONTROLS: a
/// tombstone, another batch file, a program that is not a `.cmd` and a folder
/// named like one run nothing, so they say nothing.
#[test]
fn an_atpkg_shim_reads_back_as_the_program_it_runs() {
    let tmp = Tmp::new("idle-shim");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let program = tmp
        .path()
        .join("repo")
        .join("target")
        .join("debug")
        .join("tool.exe");

    let shim = bin.join("tool.cmd");
    atpkg::platform::install_shim_to(&shim, &program).expect("atpkg lays its shim");
    assert_eq!(
        cmd_shim_target(&shim, kind(&shim)),
        Ok(Some(program.clone()))
    );
    let upper = bin.join("TOOL2.CMD");
    atpkg::platform::install_shim_to(&upper, &program).expect("atpkg lays its shim");
    assert_eq!(
        cmd_shim_target(&upper, kind(&upper)),
        Ok(Some(program.clone()))
    );

    let tomb = bin.join("gone.cmd");
    atpkg::platform::install_tombstone_shim(&tomb, "gone").expect("tombstone");
    assert_eq!(cmd_shim_target(&tomb, kind(&tomb)), Ok(None));
    let other = bin.join("other.cmd");
    std::fs::write(&other, b"@echo off\r\necho hello\r\n").expect("batch");
    assert_eq!(cmd_shim_target(&other, kind(&other)), Ok(None));
    let body = std::fs::read(&shim).expect("the shim's body");
    let not_cmd = bin.join("tool.bat");
    std::fs::write(&not_cmd, &body).expect("same body, another name");
    assert_eq!(cmd_shim_target(&not_cmd, kind(&not_cmd)), Ok(None));
    let folder = bin.join("folder.cmd");
    std::fs::create_dir(&folder).expect("folder");
    assert_eq!(cmd_shim_target(&folder, kind(&folder)), Ok(None));
}

/// A SHIM'S PROGRAM IS RECORDED AS THE CANDIDATES ARE SPELLED: resolved,
/// verbatim and in its case on disk, so a shim naming a program inside a
/// build directory — through any case, or a junction — is a link into that
/// candidate's canonical path.
#[test]
fn a_shims_program_is_recorded_as_the_candidates_are_spelled() {
    let tmp = Tmp::new("idle-shim-link");
    let build = tmp.path().join("repo").join("target");
    let program_dir = build.join("debug");
    std::fs::create_dir_all(&program_dir).expect("a build directory");
    std::fs::write(program_dir.join("tool.run"), b"a program").expect("program");
    let candidate = std::fs::canonicalize(&build).expect("a candidate's path");
    let shim = tmp.path().join("bin").join("tool.cmd");
    let as_written = |root: &Path| {
        root.join("REPO")
            .join("Target")
            .join("DEBUG")
            .join("tool.run")
    };

    let raw = as_written(tmp.path());
    assert!(
        !raw.starts_with(&candidate),
        "the shim's text is no candidate"
    );
    let link = shim_link(&shim, &raw).expect("resolved");
    assert!(
        link.starts_with(&candidate),
        "{} under {}",
        link.display(),
        candidate.display()
    );

    let alias = tmp.path().join("alias");
    atpkg::platform::atomic_symlink(&tmp.path().join("repo"), &alias).expect("a junction");
    let through = alias.join("target").join("debug").join("tool.run");
    let link = shim_link(&shim, &through).expect("resolved");
    assert!(
        link.starts_with(&candidate),
        "{} under {}",
        link.display(),
        candidate.display()
    );

    let gone = program_dir.join("gone.run");
    assert!(shim_link(&shim, &gone).is_err(), "unknown, so held");
}

/// UNKNOWN MEANS HELD: a `.cmd` that is there but cannot be read through
/// atpkg's bounded reader (here: past its bound) is an error, which keeps
/// every candidate, never "no shim".
#[test]
fn a_shim_that_cannot_be_read_is_unknown_not_absent() {
    let tmp = Tmp::new("idle-shim-unread");
    let big = tmp.path().join("big.cmd");
    std::fs::write(&big, vec![b'@'; 256 * 1024]).expect("an oversized batch file");
    let read = cmd_shim_target(&big, kind(&big));
    assert!(read.is_err(), "{read:?}");
}
