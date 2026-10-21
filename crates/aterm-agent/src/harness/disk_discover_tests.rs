// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for finding the build directories and for the build directory whose owner is gone.
//!
//! Every tree lives in its own scratch directory ([`Tmp`]): a `home/` that
//! stands in for `$HOME` (its checkouts, a Claude Code registry and
//! transcripts under `home/.claude`) and a Claude Code temp root.
//! No test reads or writes anything of the owner's: the registry is a
//! fixture whose one live record names THIS test process, and a dead one
//! names a pid no process has. `now` is moved FORWARD past the files a test
//! just wrote ([`now_after_write`]); a file that must read recent is dated
//! to `now` by hand. Nor does any test judge the machine's processes: the
//! removal of a gone owner's build directory asks which running processes
//! use it ([`unused`]), and a test's universe is the processes IT started
//! ([`among`]) or a scan it wrote — never what else the machine runs, which
//! comes and goes with whoever else is working (a process that cannot be
//! read keeps every gone owner's build directory, by design).

use super::*;
use crate::harness::disk::pressure_tests::last_used;
use crate::harness::disk::target::{Build, Judge};
use crate::harness::disk::tests::{Tmp, lay_target, now_after_write};
use crate::harness::disk::{
    Class, Config, Removed, Report, Roots as SurveyRoots, Row, Step, StopBy, Survey, TargetDir,
    Tier, Trigger, Witness, apply_auto, pass_plan, remover, remover_among, report, scan,
};
use aterm_spec::wait::{MUST_HAPPEN, within};
use aterm_uds::holders::{PathScan, scan_path_users_of};
use std::os::unix::fs::symlink;

const DAY: i64 = 86_400;
const GIB: u64 = 1024 * 1024 * 1024;

/// A session id, as Claude Code names its sessions (a UUID).
const DEAD: &str = "0dead000-0000-4000-8000-000000000001";
const DEAD_TOO: &str = "0dead000-0000-4000-8000-000000000002";
const LIVE: &str = "11fe0000-0000-4000-8000-000000000003";
/// Sessions of a Claude Code with a `CLAUDE_CONFIG_DIR` of its own.
const ELSEWHERE: &str = "0e15e000-0000-4000-8000-000000000005";
const ELSEWHERE_ENDED: &str = "0e15e000-0000-4000-8000-000000000006";
const COPIED: &str = "0e15e000-0000-4000-8000-000000000007";
/// A session of a Claude Code whose directory links its `projects/` to
/// `~/.claude`'s.
const LINKED: &str = "0e15e000-0000-4000-8000-000000000008";

/// A pid no process has (`kill(pid, 0)` answers ESRCH).
const NO_PID: u32 = 0x7fff_fff0;

/// Set the mtime of the file or directory at `path` to `at`.
fn set_mtime(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .expect("set mtime");
}

fn mkdirs(p: &Path) -> PathBuf {
    std::fs::create_dir_all(p).expect("mkdir");
    p.to_path_buf()
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).expect("canonical")
}

/// A registry record of Claude Code's (`~/.claude/sessions/<pid>.json`)
/// naming `session` for `pid`.
fn register(home: &Path, pid: u32, session: &str) {
    let dir = mkdirs(&home.join(".claude/sessions"));
    std::fs::write(
        dir.join(format!("{pid}.json")),
        format!(
            "{{\"pid\":{pid},\"sessionId\":\"{session}\",\"cwd\":\"/x\",\"version\":\"2.1.0\",\
             \"status\":\"idle\",\"statusUpdatedAt\":1,\"procStart\":\"Thu Oct  1 00:10:35 2026\",\
             \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}"
        ),
    )
    .expect("registry record");
}

/// Claude Code's temp root at `claude_tmp`, made as Claude Code makes it: `0700`,
/// whatever the test's umask (Ubuntu's 002 would leave it group-writable, a root
/// discovery refuses to search). An existing root keeps its mode.
fn claude_root(claude_tmp: &Path) {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(claude_tmp)
        .expect("Claude Code's temp root");
}

/// A Claude Code session's scratch build: `<tmp>/<project>/<session>/scratchpad/<sub>/target`,
/// with a task output beside it and its transcript under `home`. Answers the
/// build directory (canonical).
fn scratch_build(claude_tmp: &Path, home: &Path, session: &str, sub: &str) -> PathBuf {
    claude_root(claude_tmp);
    let sdir = claude_tmp.join("-proj").join(session);
    let target = lay_target(&sdir.join("scratchpad").join(sub).join("target"));
    std::fs::write(mkdirs(&sdir.join("tasks")).join("t1.output"), b"done\n").expect("task");
    let projects = mkdirs(&home.join(".claude/projects/-proj"));
    std::fs::write(projects.join(format!("{session}.jsonl")), b"{}\n").expect("transcript");
    canon(&target)
}

/// The discovery roots of the fixture under `tmp`.
fn roots_at(tmp: &Tmp) -> Roots {
    Roots {
        cwds: Vec::new(),
        home: Some(tmp.path().join("home")),
        claude_dirs: vec![tmp.path().join("home/.claude")],
        claudes: Vec::new(),
        claude_unread: None,
        agent_target_dirs: Vec::new(),
        claude_tmp: Some(tmp.path().join("claude-tmp")),
        device: None,
        volume: None,
        mounts: Vec::new(),
        skip_home_children: false,
    }
}

/// The found build directories as `(path, origin kind)`.
fn kinds(d: &Discovered) -> Vec<(PathBuf, &'static str)> {
    d.found
        .iter()
        .map(|f| {
            let k = match f.origin {
                Origin::Named => "named",
                Origin::Checkout { .. } => "checkout",
                Origin::AgentTargetDir => "agent",
                Origin::BuildCache { .. } => "build-cache",
                Origin::Bootstrap { .. } => "bootstrap",
                Origin::ClaudeScratch { .. } => "scratch",
                Origin::Linked { .. } => "linked",
            };
            (f.path.clone(), k)
        })
        .collect()
}

/// EVERY ROOT KIND, EACH BUILD DIRECTORY ONCE. An agent's
/// `CARGO_TARGET_DIR`; a working directory's own build directory and its
/// worktree root's; `$HOME`'s checkout with `target -> target.noindex` (ONE
/// directory, found by the link resolved and by its own name — reported
/// once, canonical) and a workspace one level down (`crates/target`); a
/// build directory BESIDE a worktree (`wt-parent/m1-target`) and in a git
/// worktree that lives outside `$HOME` (read from `.git/worktrees`); a
/// Claude Code session's scratch build three levels down. NEGATIVE
/// CONTROLS: a build directory three levels down a checkout, one in a hidden
/// directory, one under `~/Library`, one in a worktree whose gitdir names a
/// folder under `~/Library` (never looked into, whatever names it), one in
/// `$HOME`'s own hidden children (`~/.cargo` carries a `CACHEDIR.TAG` too, as
/// does `~/.cache/aterm/cells`, a layout nothing writes), and one deeper than
/// the scratch walk goes — none is found.
#[test]
fn every_root_kind_is_found_and_each_build_directory_once() {
    let tmp = Tmp::new("discover-roots");
    let t = tmp.path();
    let home = mkdirs(&t.join("home"));
    // $HOME's checkout: a git repository with a worktree outside $HOME.
    let repo = mkdirs(&home.join("repo"));
    mkdirs(&repo.join(".git/worktrees/wt1"));
    let wt1 = mkdirs(&t.join("elsewhere/wt1"));
    std::fs::write(wt1.join(".git"), "gitdir: x\n").expect(".git file");
    std::fs::write(
        repo.join(".git/worktrees/wt1/gitdir"),
        format!("{}\n", wt1.join(".git").display()),
    )
    .expect("gitdir");
    let wt1_target = lay_target(&wt1.join("target"));
    // A worktree in a folder of $HOME that is never looked into.
    mkdirs(&repo.join(".git/worktrees/wt2"));
    let wt2 = mkdirs(&home.join("Library/wt2"));
    std::fs::write(wt2.join(".git"), "gitdir: x\n").expect(".git file");
    std::fs::write(
        repo.join(".git/worktrees/wt2/gitdir"),
        format!("{}\n", wt2.join(".git").display()),
    )
    .expect("gitdir");
    lay_target(&wt2.join("target")); // never looked into
    let noindex = lay_target(&repo.join("target.noindex"));
    symlink("target.noindex", repo.join("target")).expect("target link");
    let crates_target = lay_target(&repo.join("crates/target"));
    lay_target(&repo.join("src/deep/x/target")); // too deep
    lay_target(&repo.join(".hidden/target")); // hidden
    let m1 = lay_target(&home.join("wt-parent/m1-target"));
    lay_target(&home.join("Library/Caches/target")); // never walked
    lay_target(&home.join(".cargo/registry")); // hidden child of $HOME
    // A working directory below a worktree root of its own.
    let work = mkdirs(&t.join("work"));
    mkdirs(&work.join(".git"));
    let work_target = lay_target(&work.join("target"));
    let sub = mkdirs(&work.join("sub"));
    let sub_target = lay_target(&sub.join("target-tippy"));
    // An agent's CARGO_TARGET_DIR.
    let agent = lay_target(&t.join("agent-target"));
    // Claude Code's temp root.
    let claude = t.join("claude-tmp");
    let scratch = scratch_build(&claude, &home, DEAD, "a/b");
    lay_target(
        &claude
            .join("-proj")
            .join(DEAD)
            .join("scratchpad/1/2/3/4/target"),
    ); // deeper than the scratch walk
    // `~/.cache/aterm/cells`: a layout nothing writes, never searched.
    lay_target(&home.join(".cache/aterm/cells/wt-0123/linux"));

    let roots = Roots {
        cwds: vec![sub.clone()],
        agent_target_dirs: vec![agent.clone(), PathBuf::from("relative/target")],
        ..roots_at(&tmp)
    };
    let d = discover(&roots, Budget::SHIPPED);
    assert!(d.notes.is_empty(), "{:?}", d.notes);
    let mut got = kinds(&d);
    got.sort();
    let mut want = vec![
        (canon(&agent), "agent"),
        (canon(&sub_target), "checkout"),
        (canon(&work_target), "checkout"),
        (scratch, "scratch"),
        (canon(&noindex), "checkout"),
        (canon(&crates_target), "checkout"),
        (canon(&wt1_target), "checkout"),
        (canon(&m1), "checkout"),
    ];
    want.sort();
    assert_eq!(got, want);
    // The scratch build carries its session.
    for f in &d.found {
        if let Origin::ClaudeScratch { session, .. } = &f.origin {
            assert_eq!(session, DEAD);
        }
    }
}

/// A spent budget stops the search and says so, keeping what it found.
#[test]
fn a_search_cut_short_keeps_what_it_found_and_says_so() {
    let tmp = Tmp::new("discover-budget");
    let home = mkdirs(&tmp.path().join("home"));
    for n in ["a", "b", "c"] {
        lay_target(&home.join(n).join("target"));
    }
    let d = discover(
        &roots_at(&tmp),
        Budget {
            found: 2,
            ..Budget::SHIPPED
        },
    );
    assert_eq!(d.found.len(), 2);
    assert!(
        d.notes.iter().any(|n| n.contains("stopped short")),
        "{:?}",
        d.notes
    );
}

/// An x.py checkout's `build/<host>/` gives only `stage<N>-<kind>` (N of 1 or more) and
/// `bootstrap-tools` with a signed `CACHEDIR.TAG`: never a sysroot, stage0, `llvm` or a
/// `-bin`/`-sysroot` tree, never through the `build/host` link, and the marker walk never
/// enters `build/`.
#[test]
fn an_xpy_checkout_gives_its_bootstrap_build_dirs_and_never_a_sysroot() {
    let tmp = Tmp::new("discover-xpy");
    let home = mkdirs(&tmp.path().join("home"));
    let repo = mkdirs(&home.join("trust"));
    std::fs::write(repo.join("x.py"), b"#!/usr/bin/env python3\n").expect("x.py");
    std::fs::write(mkdirs(&repo.join("scripts")).join("slim_build.py"), b"").expect("slim");
    let host = mkdirs(&repo.join("build/aarch64-apple-darwin"));
    symlink("aarch64-apple-darwin", repo.join("build/host")).expect("host link");
    let mut want = Vec::new();
    for n in [
        "stage1-rustc",
        "bootstrap-tools",
        "stage1-tools",
        "stage2-tools",
    ] {
        want.push(canon(&lay_target(&host.join(n))));
    }
    for n in [
        "stage2",
        "stage0",
        "stage0-rustc",
        "llvm",
        "stage2-tools-bin",
        "stage1-sysroot",
    ] {
        lay_target(&host.join(n));
    }
    let std_dir = mkdirs(&host.join("stage1-std"));
    std::fs::write(std_dir.join(RUSTC_INFO), b"{}").expect("rustc info");
    let bootstrap = mkdirs(&repo.join("build/bootstrap/debug"));
    std::fs::write(repo.join("build/bootstrap").join(RUSTC_INFO), b"{}").expect("info");
    let _ = bootstrap;

    let d = discover(&roots_at(&tmp), Budget::SHIPPED);
    let mut got: Vec<PathBuf> = d.found.iter().map(|f| f.path.clone()).collect();
    got.sort();
    want.sort();
    assert_eq!(got, want);
    assert!(d.found.iter().all(|f| f.origin
        == Origin::Bootstrap {
            repo: canon(&repo),
            slim: true
        }));
    // The name rule, both directions.
    for yes in [
        "stage1-rustc",
        "stage2-tools",
        "stage1-std",
        "stage12-codegen",
        "bootstrap-tools",
    ] {
        assert!(bootstrap_cargo_dir(yes), "{yes}");
    }
    for no in [
        "stage0",
        "stage1",
        "stage2",
        "stage0-rustc",
        "stage0-sysroot",
        "stage2-tools-bin",
        "stage1-sysroot",
        "llvm",
        "genesis-stage0",
        "stage-rustc",
        "stage1-",
        "native",
    ] {
        assert!(!bootstrap_cargo_dir(no), "{no}");
    }
}

/// The Rust build cache under `$HOME` is searched: each marked `.cargo-build`, however deep,
/// is a build directory with no owner that can end. Negative controls: an unmarked
/// `.cargo-build`, a marked folder of another name, and one behind a linked folder.
#[test]
fn the_rust_build_cache_gives_each_workspace_build_directory() {
    let tmp = Tmp::new("discover-build-cache");
    let home = mkdirs(&tmp.path().join("home"));
    let cache = atpkg::build_cache::cache_root(&home);
    let one = lay_target(&cache.join("Users/a/proj/.cargo-build"));
    let two = lay_target(&cache.join("Users/a/proj/nested/ws/.cargo-build"));
    mkdirs(&cache.join("Users/a/unmarked/.cargo-build/debug"));
    lay_target(&cache.join("Users/a/other-name"));
    let elsewhere = lay_target(&tmp.path().join("elsewhere/x/.cargo-build"));
    std::os::unix::fs::symlink(tmp.path().join("elsewhere"), cache.join("Users/linked"))
        .expect("link");

    let d = discover(&roots_at(&tmp), Budget::SHIPPED);
    assert!(d.notes.is_empty(), "{:?}", d.notes);
    let found = kinds(&d);
    for dir in [&one, &two] {
        assert!(
            found.contains(&(canon(dir), "build-cache")),
            "{}: {found:?}",
            dir.display()
        );
    }
    assert!(
        !found.iter().any(|(p, _)| *p == canon(&elsewhere)),
        "{found:?}"
    );
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        d.found
            .iter()
            .all(|f| !matches!(f.origin, Origin::BuildCache { .. }) || f.origin.ownable())
    );
}

/// EACH CLAUDE CODE'S HOME, FOR THE ACCOUNT STATS' COUNT OF THE SESSIONS ON
/// A HOME ([`RunningClaude::profile`]): a Claude Code with
/// `CLAUDE_CONFIG_DIR` unset is on its user's `~/.claude.json`; one under a
/// set `CLAUDE_CONFIG_DIR` on that directory's own; a relative one on the
/// one under its working directory. NEGATIVE CONTROLS: a process kept only
/// because its directory was not read (no Claude Code by the kernel's name)
/// is on none, and with no user home known none is named.
#[test]
fn each_running_claude_code_names_the_home_it_runs_in() {
    let home = PathBuf::from("/u");
    let store = "/opt/pkg/store/claude/7/bin/claude";
    let run = |config: Option<&str>, exe: &str, cwd: Option<&str>| Process {
        pid: 4242,
        exe: Some(Exe::At(exe.into())),
        exec: Some(Exec {
            path: exe.to_owned(),
            argv: vec![exe.to_owned()],
            config: config.map(str::to_owned),
            home: Some("/u".to_owned()),
            stored_login: true,
        }),
        cwd: cwd.map(PathBuf::from),
        started: None,
    };
    let profile = |p: Process, home: Option<&Path>| {
        claude_census([p], home)
            .expect("read")
            .claudes
            .first()
            .map(|c| c.profile.clone())
    };
    assert_eq!(
        profile(run(None, store, Some("/w")), Some(&home)),
        Some(Some(home.join(".claude.json")))
    );
    assert_eq!(
        profile(run(Some("/srv/b"), store, Some("/w")), Some(&home)),
        Some(Some(PathBuf::from("/srv/b/.claude.json")))
    );
    assert_eq!(
        profile(run(Some("rel"), store, Some("/w")), Some(&home)),
        Some(Some(PathBuf::from("/w/rel/.claude.json")))
    );
    // NEGATIVE CONTROLS: kept unread, not named a Claude Code; no home.
    assert_eq!(
        profile(run(Some("rel"), "/bin/zsh", None), Some(&home)),
        Some(None)
    );
    assert_eq!(profile(run(None, store, Some("/w")), None), Some(None));
}

/// THE SESSION'S OWNER: GONE ONLY WHEN NOTHING NAMES IT AND NOTHING OF IT
/// WAS WRITTEN. A session the registry names for a running process (this
/// test's own pid) is live; one no record names, every file of it written
/// days ago, is gone. NEGATIVE CONTROLS: a session no record names whose
/// task output was written an hour ago is live (`claude -p` keeps no record),
/// and so is one whose transcript was; a record naming a pid no process has
/// does not keep a session live; and a registry that cannot be read whole —
/// a record of a running pid that is not Claude Code's shape — makes NO
/// session gone. A Claude Code with a `CLAUDE_CONFIG_DIR` of its own keeps
/// its registry and transcripts there: known only `~/.claude`, neither its
/// running session nor its ended one is called ended; known its directory
/// too, the running one is live and the ended one gone. WHO MAY HOLD A
/// SESSION is read from the KERNEL ([`claude_census`]): a Claude Code whose
/// registry lives where no census looks — started as this account, its
/// settings moved its directory to `alt/.claude` after its exec, whose
/// `projects/` links to `~/.claude`'s — keeps every session of the project
/// it works in (from its own worktree too, or with its working directory
/// not read), at the survey and at the removal; a Claude Code by any
/// install's name is one. NEGATIVE CONTROLS: none running there, one in
/// another project, a shell, or one a registry read here PINS to its own
/// session (its pid at its start — not a pid reused) leaves it ended; a
/// process that may be a Claude Code but cannot be read makes no session
/// ended — and one caught mid-spawn is read again, judged once its exec
/// completes. The census reads, for reach, the directory each process was
/// started with — a renamed Claude Code's registry names its session there;
/// aterm's own identities' in `~/Library` too, never another place of
/// `~/Library` nor one that climbs out by `..` — and none is a reason to
/// refuse.
#[test]
fn a_session_is_gone_only_when_nothing_names_it_and_nothing_of_it_was_written() {
    let tmp = Tmp::new("discover-owners");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    for s in [DEAD, LIVE, DEAD_TOO] {
        scratch_build(&claude, &home, s, "x");
    }
    let quiet_session = "0dead000-0000-4000-8000-000000000004";
    scratch_build(&claude, &home, quiet_session, "x");
    register(&home, std::process::id(), LIVE);
    register(&home, NO_PID, DEAD);
    // Two sessions of a Claude Code with a CLAUDE_CONFIG_DIR of its own
    // (`other/.claude`, as `spawn identity=` starts one): the temp root is
    // every Claude Code's of this uid, the registry and transcripts are that
    // directory's. One runs (this process), one ended.
    let other = tmp.path().join("other");
    for s in [ELSEWHERE, ELSEWHERE_ENDED] {
        scratch_build(&claude, &other, s, "x");
    }
    register(&other, std::process::id(), ELSEWHERE);
    register(&other, NO_PID, ELSEWHERE_ENDED);
    // A session running under a third directory (this process), whose
    // transcript is not there (pruned) but a copy of it is in `other`.
    let third = tmp.path().join("third");
    scratch_build(&claude, &other, COPIED, "x");
    register(&third, std::process::id(), COPIED);
    // A session running (this process) under `alt/.claude`, which nobody named, whose
    // `projects/` is `~/.claude`'s, linked: its transcript is in a known directory, its
    // registry is not.
    let alt = tmp.path().join("alt");
    scratch_build(&claude, &home, LINKED, "x");
    symlink(
        home.join(".claude/projects"),
        mkdirs(&alt.join(".claude")).join("projects"),
    )
    .expect("linked projects");
    register(&alt, std::process::id(), LINKED);
    let now = now_after_write(3);
    // DEAD_TOO wrote a task output an hour ago.
    let recent = claude.join("-proj").join(DEAD_TOO).join("tasks/t1.output");
    set_mtime(&recent, now - 3_600);
    // quiet_session's transcript was written an hour ago.
    let transcript = home.join(format!(".claude/projects/-proj/{quiet_session}.jsonl"));
    set_mtime(&transcript, now - 3_600);

    let d = discover(&roots_at(&tmp), Budget::SHIPPED);
    let mut owners = Owners::new(
        Some(home.clone()),
        vec![home.join(".claude")],
        Vec::new(),
        None,
        now,
        DAY,
    );
    let verdict = |owners: &mut Owners, session: &str| {
        let f = d
            .found
            .iter()
            .find(|f| matches!(&f.origin, Origin::ClaudeScratch { session: s, .. } if s == session))
            .expect("found");
        owners.of(&f.origin)
    };
    assert!(
        matches!(verdict(&mut owners, DEAD), Owned::Gone { ref evidence, .. } if evidence.contains("ended")),
        "{:?}",
        verdict(&mut owners, DEAD)
    );
    assert!(
        matches!(verdict(&mut owners, LIVE), Owned::Live(ref why) if why.contains("is running")),
        "{:?}",
        verdict(&mut owners, LIVE)
    );
    assert!(
        matches!(verdict(&mut owners, DEAD_TOO), Owned::Live(ref why) if why.contains("wrote")),
        "{:?}",
        verdict(&mut owners, DEAD_TOO)
    );
    assert!(
        matches!(verdict(&mut owners, quiet_session), Owned::Live(_)),
        "{:?}",
        verdict(&mut owners, quiet_session)
    );
    // Knowing only `~/.claude`, neither CLAUDE_CONFIG_DIR session is ended:
    // no directory known holds its transcript.
    for s in [ELSEWHERE, ELSEWHERE_ENDED] {
        assert!(
            matches!(verdict(&mut owners, s), Owned::Live(ref why) if why.contains("transcript")),
            "{s}: {:?}",
            verdict(&mut owners, s)
        );
    }
    // Knowing that directory too (an identity's, a supervised agent's
    // CLAUDE_CONFIG_DIR), each is judged by its own registry and transcript:
    // the running one is live, the ended one gone — and one any known
    // registry names running is live wherever its transcript is.
    let dirs = vec![
        home.join(".claude"),
        other.join(".claude"),
        third.join(".claude"),
    ];
    let mut known = Owners::new(Some(home.clone()), dirs, Vec::new(), None, now, DAY);
    assert!(
        matches!(verdict(&mut known, COPIED), Owned::Live(ref why) if why.contains("is running")),
        "{:?}",
        verdict(&mut known, COPIED)
    );
    assert!(
        matches!(verdict(&mut known, ELSEWHERE), Owned::Live(ref why) if why.contains("is running")),
        "{:?}",
        verdict(&mut known, ELSEWHERE)
    );
    assert!(
        matches!(verdict(&mut known, ELSEWHERE_ENDED), Owned::Gone { .. }),
        "{:?}",
        verdict(&mut known, ELSEWHERE_ENDED)
    );
    // Who may hold the linked session, from the kernel: its Claude Code runs, started as this
    // account, so the census names `~/.claude`, not `alt/.claude` where its settings moved
    // its registry.
    let judged = |census: ClaudeCensus| {
        let (running, unread) = match census {
            Ok(running) => (running, None),
            Err(why) => (Running::default(), Some(why)),
        };
        let dirs = claude_dirs(Some(&home), running.dirs);
        verdict(
            &mut Owners::new(Some(home.clone()), dirs, running.claudes, unread, now, DAY),
            LINKED,
        )
    };
    let text = |p: &Path| Some(p.display().to_string());
    // `register`'s procStart, its spaces squashed: this process's records.
    const STARTED: &str = "Thu Oct 1 00:10:35 2026";
    // A process: its pid, the kernel's executable, its exec path and argv,
    // where it works and when it started.
    let store = "/opt/pkg/store/claude/7/bin/claude";
    let native = "/u/.local/share/claude/versions/2.1.281";
    let process =
        |pid: u32, run: [&str; 2], argv: &[&str], cwd: Option<&str>, started: &str| Process {
            pid,
            exe: Some(Exe::At(run[0].into())),
            exec: Some(Exec {
                path: run[1].to_owned(),
                argv: argv.iter().map(|a| (*a).to_owned()).collect(),
                config: None,
                home: text(&home),
                stored_login: true,
            }),
            cwd: cwd.map(PathBuf::from),
            started: Some(started.to_owned()),
        };
    let Owned::Gone { owner, .. } = judged(Ok(Running::default())) else {
        panic!("{:?}", judged(Ok(Running::default())));
    };
    // The fixture's project folder is `-proj`: launched in `/proj`.
    let claude = process(4242, [store; 2], &[store], Some("/proj"), STARTED);
    let running = claude_census([claude], Some(&home));
    assert_eq!(
        running.as_ref().map(|r| r.dirs.clone()),
        Ok(vec![home.join(".claude")])
    );
    let node = "/usr/bin/node";
    for (pid, run, argv, cwd, started, keeps) in [
        (4242, [store; 2], &[store][..], Some("/proj"), STARTED, true),
        (
            4242,
            [store; 2],
            &[store],
            Some("/proj/.claude/worktrees/w"),
            STARTED,
            true,
        ),
        (4242, [store; 2], &[store], None, STARTED, true),
        (
            4242,
            [native, "/u/bin/cc"],
            &["cc"],
            Some("/proj"),
            STARTED,
            true,
        ),
        (
            4242,
            [node; 2],
            &["node", "/n/@anthropic-ai/claude-code/cli.js"],
            Some("/proj"),
            STARTED,
            true,
        ),
        (
            std::process::id(),
            [store; 2],
            &[store],
            Some("/proj"),
            "Fri Oct 2 01:02:03 2026",
            true,
        ),
        (4242, [store; 2], &[store], Some("/other"), STARTED, false),
        (
            4242,
            [store; 2],
            &["ugrep", "-G"],
            Some("/proj"),
            STARTED,
            false,
        ),
        (
            4242,
            ["/bin/zsh"; 2],
            &["-zsh"],
            Some("/proj"),
            STARTED,
            false,
        ),
        (
            std::process::id(),
            [store; 2],
            &[store],
            Some("/proj"),
            STARTED,
            false,
        ),
    ] {
        let census = claude_census([process(pid, run, argv, cwd, started)], Some(&home));
        let live = judged(census);
        assert_eq!(
            matches!(live, Owned::Live(ref why) if why.contains(&format!("process {pid} runs in"))),
            keeps,
            "{argv:?} in {cwd:?}: {live:?}"
        );
    }
    // Neither a system executable nor an exited process may be one; a process that may be a
    // Claude Code but whose exec record is hidden (its executable named or not) makes no
    // session ended.
    let hidden = |exe: Exe| Process {
        pid: 4242,
        exe: Some(exe),
        ..Process::default()
    };
    let quiet = claude_census(
        [hidden(Exe::At("/bin/zsh".into())), hidden(Exe::Exited)],
        Some(&home),
    );
    assert_eq!(quiet, Ok(Running::default()));
    // One caught mid-spawn — no exec record yet, the kernel still naming its
    // parent's executable — is read again: once its exec completes it is
    // told (a Claude Code in the project keeps it), and one that stays
    // unread is refused.
    let parent = Exe::At("/opt/homebrew/bin/python3".into());
    let born = process(4242, [store; 2], &[store], Some("/proj"), STARTED);
    let mut reads = 0;
    let (exec, exe) = settled(
        || {
            reads += 1;
            if reads < 3 {
                (None, parent.clone())
            } else {
                (born.exec.clone(), Exe::At(store.into()))
            }
        },
        |_| {},
    );
    let spawned = Process {
        exec,
        exe: Some(exe),
        ..born
    };
    let live = judged(claude_census([spawned], Some(&home)));
    assert!(
        matches!(live, Owned::Live(ref why) if why.contains("process 4242 runs in /proj")),
        "{live:?}"
    );
    let (exec, exe) = settled(|| (None, parent.clone()), |_| {});
    let unsettled = Process {
        exec,
        ..hidden(exe)
    };
    for refused in [
        claude_census([hidden(Exe::At("/opt/x/bin/claude".into()))], Some(&home)),
        claude_census([hidden(Exe::Unnamed)], Some(&home)),
        claude_census([unsettled], Some(&home)),
    ] {
        let live = judged(refused);
        assert!(
            matches!(live, Owned::Live(ref why) if why.contains("no session is called ended")),
            "{live:?}"
        );
    }
    // For reach, each process's own directory: aterm's identity's (in `~/Library` on macOS)
    // is read; another place of `~/Library`, or one that climbs out of aterm's by `..`, is
    // not, and none refuses the census. A relative one is under the process's working
    // directory, as Claude Code resolves it.
    let state = aterm_types::dirs::resolve_state_dir(
        None,
        aterm_types::dirs::StatePlatform {
            home: Some(home.clone()),
            ..aterm_types::dirs::StatePlatform::default()
        },
    )
    .expect("aterm's state directory");
    let identity = state.join("identities/work/.claude");
    let started_in = |config: PathBuf| {
        let mut p = process(4242, [store; 2], &[store], Some("/proj"), STARTED);
        if let Some(exec) = p.exec.as_mut() {
            exec.config = text(&config);
        }
        p
    };
    let reach = claude_census(
        [
            started_in(identity.clone()),
            started_in(home.join("Library/Mail/.claude")),
            started_in(state.join("../../Mail/.claude")),
            started_in(PathBuf::from("rel/.claude")),
        ],
        Some(&home),
    );
    // Two folders up from aterm's state is `~/Library` on macOS, never looked
    // into; elsewhere (`~/.local/state/aterm`) it is a plain folder of `$HOME`
    // (`~/.local`), read as any other.
    let climbed = state
        .parent()
        .and_then(Path::parent)
        .expect("two folders above aterm's state")
        .join("Mail/.claude");
    let mut read = vec![identity];
    if !cfg!(target_os = "macos") {
        read.push(climbed);
    }
    read.push(PathBuf::from("/proj/rel/.claude"));
    assert_eq!(reach.map(|r| r.dirs), Ok(read));
    // And the directory ANY process was started with: a Claude Code the
    // kernel does not name one (a copy renamed `cc-work`, started with
    // `CLAUDE_CONFIG_DIR=alt/.claude`) keeps its registry there, which names
    // the linked session running.
    let renamed = Process {
        pid: 4243,
        exe: Some(Exe::At("/u/bin/cc-work".into())),
        exec: Some(Exec {
            path: "/u/bin/cc-work".to_owned(),
            argv: vec!["cc-work".to_owned()],
            config: text(&alt.join(".claude")),
            home: text(&home),
            stored_login: true,
        }),
        ..Process::default()
    };
    let live = judged(claude_census([renamed], Some(&home)));
    assert!(
        matches!(live, Owned::Live(ref why) if why.contains("is running")),
        "{live:?}"
    );
    // The same copy started with its directory where the watch never looks
    // (`~/Library`, not aterm's): its registry is not read, so it keeps the
    // project it works in.
    let guarded = Process {
        pid: 4243,
        exe: Some(Exe::At("/u/bin/cc-work".into())),
        exec: Some(Exec {
            path: "/u/bin/cc-work".to_owned(),
            argv: vec!["cc-work".to_owned()],
            config: text(&home.join("Library/Work/.claude")),
            home: text(&home),
            stored_login: true,
        }),
        cwd: Some(PathBuf::from("/proj")),
        ..Process::default()
    };
    let live = judged(claude_census([guarded], Some(&home)));
    assert!(
        matches!(live, Owned::Live(ref why) if why.contains("process 4243 runs in /proj")),
        "{live:?}"
    );
    // The removal reads them again: a Claude Code started since in the
    // project keeps it, one whose directory names the session running does,
    // and so does one that cannot be read.
    let census = |running: ClaudeCensus| move |_: Option<&Path>| running.clone();
    assert_eq!(
        still_gone(&owner, now, DAY, &census(Ok(Running::default()))),
        Ok(())
    );
    let started = still_gone(&owner, now, DAY, &census(running));
    assert!(started.is_err_and(|why| why.contains("runs in /proj")));
    let named = Running {
        dirs: vec![alt.join(".claude")],
        ..Running::default()
    };
    let resumed = still_gone(&owner, now, DAY, &census(Ok(named)));
    assert!(resumed.is_err_and(|why| why.contains("is running")));
    let unread = still_gone(&owner, now, DAY, &census(Err("x".to_owned())));
    assert!(unread.is_err_and(|why| why.contains("no session is called ended: x")));
    // An unreadable registry: a running pid's record that is not Claude
    // Code's shape. No session is gone.
    std::fs::write(
        home.join(format!(".claude/sessions/{}.json", std::process::id())),
        b"{",
    )
    .expect("broken record");
    let mut fresh = Owners::new(
        Some(home.clone()),
        vec![home.join(".claude")],
        Vec::new(),
        None,
        now,
        DAY,
    );
    assert!(
        matches!(verdict(&mut fresh, DEAD), Owned::Live(ref why) if why.contains("registry")),
        "{:?}",
        verdict(&mut fresh, DEAD)
    );
}

// ---------------------------------------------------------------------------
// The report and the pass
// ---------------------------------------------------------------------------

/// A config with the floor at 10 GiB and the dead-build switch as given.
fn cfg(dead: bool) -> Config {
    Config {
        dead_build_dirs: dead,
        ..Config::default()
    }
}

/// Survey what `roots` discovers under `tmp` at `now` with `free` bytes free.
fn survey_at(tmp: &Tmp, roots: &Roots, now: i64, free: u64, config: Config) -> (Survey, Report) {
    let d = discover(roots, Budget::SHIPPED);
    let s = scan(
        &SurveyRoots {
            volume: Some(tmp.path().to_path_buf()),
            found: d.found,
            found_notes: d.notes,
            home: roots.home.clone(),
            claude_dirs: roots.claude_dirs.clone(),
            claudes: roots.claudes.clone(),
            claude_unread: roots.claude_unread.clone(),
            // The machine's own mount table, read whole: nothing is mounted
            // under the test's scratch directory.
            mounts: Some(aterm_sysprobe::volumes::table()),
            placing: roots.home.as_deref().and_then(super::placing_now),
            ..SurveyRoots::default()
        },
        now,
        Trigger::Tick,
        config,
        Some(free),
    );
    let rep = report(&s, config);
    (s, rep)
}

/// A re-measure that answers `script` in turn (its last figure after that).
fn scripted(script: Vec<u64>, reads: &mut usize) -> impl FnMut() -> Option<u64> + '_ {
    move || {
        let got = script
            .get(*reads)
            .or(script.last())
            .copied()
            .expect("a script");
        *reads += 1;
        Some(got)
    }
}

/// A test's process universe: the processes `pids` (ones it started) and no
/// other, each read for real ([`scan_path_users_of`]).
fn among(pids: Vec<u32>) -> impl Fn(&[PathBuf], &[u32]) -> std::io::Result<PathScan> {
    move |dirs, skip| scan_path_users_of(&pids, dirs, skip)
}

/// The shipped judge at `now`, on any device (a pressure row carries its
/// own).
fn judge_at(now: i64) -> Judge {
    Judge {
        now,
        threshold_days: crate::harness::disk::DEFAULT_TARGET_STALE_DAYS,
        device: None,
        volume: None,
    }
}

/// The real remover at `now`, among the running processes `processes` reads;
/// none of them names a Claude Code directory the survey did not read.
fn real_with(
    now: i64,
    processes: impl Fn(&[PathBuf], &[u32]) -> std::io::Result<PathScan>,
) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    remover_among(
        judge_at(now),
        processes,
        |_| Ok(Running::default()),
        crate::harness::disk::idle::Sessions::default,
        aterm_sysprobe::volumes::table,
    )
}

/// The real remover at `now`, among the processes `pids` (the test's own).
fn real_among(now: i64, pids: Vec<u32>) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    real_with(now, among(pids))
}

/// The real remover at `now`, where no process of the test's runs.
fn real(now: i64) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    real_among(now, Vec::new())
}

/// BELOW THE FLOOR, ENDED SESSIONS' BUILDS GO WHOLE FIRST (TIER 1, THE
/// LONGEST UNWRITTEN FIRST, ALL OF THEM: NOBODY BUILDS THERE AGAIN), THEN THE
/// IDLE CACHES (TIER 2), THEN THE LEAST RECENTLY USED PROFILES (TIER 4), EACH
/// TIER STOPPED AT ITS OWN TARGET. Four build directories: a checkout's idle
/// profile (two days unbuilt), two ended sessions' scratch builds (five and
/// three days unwritten), and a live checkout's profile built three hours
/// ago. The report lists them in the order the pass takes them, each with its
/// tier. Tier 1 takes both ended builds with no re-measure; free space
/// measured before the idle cache is short of the warning, so it goes; before
/// the recent profile it reads the floor plus the margin, so tier 4 stops
/// there. The ended sessions' build directories are GONE, root and all; the
/// idle checkout keeps everything but its `incremental/`; the live checkout's
/// cache is untouched. The journal says it as it happened.
#[test]
fn below_the_floor_ended_sessions_builds_go_whole_first_then_idle_then_recent_caches() {
    let tmp = Tmp::new("discover-pass");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(6);
    let idle = canon(&lay_target(&home.join("idle/target")));
    last_used(&idle.join("debug"), now - 2 * DAY);
    let live = canon(&lay_target(&home.join("live/target")));
    last_used(&live.join("debug"), now - 3 * 3_600);
    let old = scratch_build(&claude, &home, DEAD, "a");
    last_used(&old.join("debug"), now - 5 * DAY);
    let newer = scratch_build(&claude, &home, DEAD_TOO, "b");
    last_used(&newer.join("debug"), now - 3 * DAY);
    mkdirs(&home.join(".claude/sessions")); // a registry naming nobody

    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    let order: Vec<(&str, PathBuf, Option<u8>)> = rep
        .rows
        .iter()
        .filter(|r| r.class == Class::CargoTargets)
        .map(|r| (r.witness.kind(), r.path.clone(), r.tier().map(Tier::number)))
        .collect();
    assert_eq!(
        order,
        [
            ("owner-gone", old.clone(), Some(1)),
            ("owner-gone", newer.clone(), Some(1)),
            ("stale-build-dir", idle.clone(), Some(2)),
            ("least-recently-used", live.join("debug"), Some(4)),
        ],
        "{:#?}",
        rep.notes
    );
    assert!(
        rep.rows[0].witness.describe().contains("ended"),
        "{}",
        rep.rows[0].witness.describe()
    );
    assert!(
        rep.rows[0].line().contains("tier=1"),
        "{}",
        rep.rows[0].line()
    );

    let target = cfg(true).pressure_target_bytes();
    let mut reads = 0;
    let mut journal: Vec<String> = Vec::new();
    let done = apply_auto(
        &rep,
        None,
        &mut real(now),
        &mut scripted(vec![GIB, target], &mut reads),
        &mut |step| {
            journal.push(match step {
                Step::Removing(r) => format!("removing {}", r.path.display()),
                Step::Removed(r, _) => format!("removed {}", r.path.display()),
                Step::Denied(d) => format!("denied {}", d.describe()),
                Step::Stopped(s) => format!("stopped {}", s.by.as_str()),
            });
            true
        },
    )
    .expect("below the floor");
    assert_eq!(
        journal,
        [
            format!("removing {}", old.display()),
            format!("removed {}", old.display()),
            format!("removing {}", newer.display()),
            format!("removed {}", newer.display()),
            format!("removing {}", idle.display()),
            format!("removed {}", idle.display()),
            "stopped measured".to_owned(),
        ]
    );
    assert_eq!(reads, 2, "measured before each row of a tier with a target");
    let stop = done.stopped.expect("stopped");
    assert_eq!(
        (stop.by, stop.left, stop.tier),
        (StopBy::Measured, 1, Some(4))
    );
    for gone in [&old, &newer] {
        assert!(
            !gone.exists(),
            "the ended session's build directory is gone whole"
        );
        assert!(
            gone.parent().expect("parent").exists(),
            "its scratchpad stays"
        );
    }
    assert!(!idle.join("debug/incremental").exists());
    assert!(
        idle.join("debug/deps/liblay.rlib").exists(),
        "deps/ is never an idle row's"
    );
    assert!(live.join("debug/incremental").exists());
    let freed = done
        .removed
        .iter()
        .find(|(r, _)| r.path == old)
        .map(|(_, r)| r.bytes)
        .expect("removed");
    assert!(
        freed >= 8192,
        "the unlinks counted what they released: {freed}"
    );
}

/// AT `LOW` (below the warning, above the floor) the same ended build goes
/// whole and the idle cache goes, measured again; no recent profile is a row
/// at all. NEGATIVE CONTROL: at `ok` neither goes whole — the report notes
/// that it would once the volume is low.
#[test]
fn at_low_an_ended_sessions_build_goes_whole_and_no_recent_profile_is_a_row() {
    let tmp = Tmp::new("discover-low");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(6);
    let live = canon(&lay_target(&home.join("live/target")));
    last_used(&live.join("debug"), now - 3 * 3_600);
    let old = scratch_build(&claude, &home, DEAD, "a");
    last_used(&old.join("debug"), now - 5 * DAY);
    mkdirs(&home.join(".claude/sessions"));
    let low = 20 * GIB;
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, low, cfg(true));
    let kinds: Vec<&str> = rep
        .rows_of(Class::CargoTargets)
        .iter()
        .map(|r| r.witness.kind())
        .collect();
    assert_eq!(kinds, ["owner-gone"], "{:#?}", rep.rows);
    let done = apply_auto(&rep, None, &mut real(now), &mut || Some(low), &mut |_| true)
        .expect("low admits tier 1");
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(!old.exists());
    assert!(live.join("debug/incremental").exists());

    let kept = scratch_build(&claude, &home, DEAD_TOO, "b");
    last_used(&kept.join("debug"), now - 5 * DAY);
    let (_, ok) = survey_at(&tmp, &roots_at(&tmp), now, 500 * GIB, cfg(true));
    assert!(
        !ok.rows
            .iter()
            .any(|r| matches!(r.witness, Witness::OwnerGone { .. })),
        "{:#?}",
        ok.rows
    );
    assert_eq!(pass_plan(&ok), None);
    assert!(
        ok.hogs
            .iter()
            .any(|h| h.why.contains("once free space is below")),
        "{:#?}",
        ok.hogs
    );
}

/// WHAT KEEPS A WHOLE DIRECTORY: a live session, a session whose build ran
/// two hours ago (its profile's clock is a write of the session's), a build
/// holding a lock — each is judged as any other build directory then (its
/// idle and least-recently-used caches only), and of a gone owner's the
/// report says why it does not go whole. And at the REMOVAL: a build that
/// took a lock since the survey and a session resumed since (its registry
/// record back) each refuse it, nothing deleted; NEGATIVE CONTROL, with the
/// record gone again the same row goes.
#[test]
fn a_live_owner_a_held_lock_or_a_recent_build_keeps_the_whole_directory() {
    let tmp = Tmp::new("discover-keep");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(4);
    let live = scratch_build(&claude, &home, LIVE, "a");
    register(&home, std::process::id(), LIVE);
    let held = scratch_build(&claude, &home, DEAD, "b");
    let recent = scratch_build(&claude, &home, DEAD_TOO, "c");
    last_used(&recent.join("debug"), now - 2 * 3_600);
    let build = Build::holds(&held.join("debug/.cargo-lock"));
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    drop(build);
    assert!(
        !rep.rows
            .iter()
            .any(|r| matches!(r.witness, Witness::OwnerGone { .. })),
        "{:#?}",
        rep.rows
    );
    let note = |p: &Path| {
        rep.notes
            .iter()
            .find(|n| n.contains(&p.display().to_string()) && n.contains("does not go whole"))
            .cloned()
    };
    assert!(
        note(&held).is_some_and(|n| n.contains("a build holds")),
        "{:#?}",
        rep.notes
    );
    assert!(
        note(&recent).is_none(),
        "a live owner is no question: {:#?}",
        rep.notes
    );
    assert!(
        note(&live).is_none(),
        "a live owner is no question: {:#?}",
        rep.notes
    );
    // The recently built one gives its cache under pressure, as any other.
    assert!(rep.rows.iter().any(|r| r.path == recent.join("debug")
        && matches!(r.witness, Witness::LeastRecentlyUsed { .. })));
    // The live session's build is any other: its idle profile's cache is a row.
    assert!(
        rep.rows
            .iter()
            .any(|r| r.path == live && matches!(r.witness, Witness::StaleBuildDir { .. }))
    );

    // At the removal. A clean survey of an ended session's build...
    let tmp = Tmp::new("discover-refuse");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let gone = scratch_build(&claude, &home, DEAD, "a");
    mkdirs(&home.join(".claude/sessions"));
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    let row: Row = rep
        .rows
        .iter()
        .find(|r| matches!(r.witness, Witness::OwnerGone { .. }))
        .cloned()
        .expect("a whole row");
    // ...then a build takes a lock: refused, nothing deleted.
    let build = Build::holds(&gone.join("debug/.cargo-lock"));
    let err = real(now)(&row).expect_err("a build holds a lock");
    drop(build);
    assert!(err.to_string().contains("a build holds"), "{err}");
    assert!(gone.join("debug/incremental").exists() && gone.join("CACHEDIR.TAG").exists());
    // ...then the session is resumed: refused, nothing deleted.
    register(&home, std::process::id(), DEAD);
    let err = real(now)(&row).expect_err("the session runs again");
    assert!(err.to_string().contains("is running"), "{err}");
    assert!(gone.join("debug/deps/liblay.rlib").exists());
    // Negative control: with the record gone again, the same row goes.
    std::fs::remove_file(home.join(format!(".claude/sessions/{}.json", std::process::id())))
        .expect("unregister");
    let removed = real(now)(&row).expect("the owner is gone again");
    assert_eq!(removed.units, std::slice::from_ref(&gone));
    assert!(!gone.exists());
}

/// Above the floor, or with `dead_build_dirs` off, an ended session's build is like any other
/// (its idle `incremental/` is the row) and its hog line says why the rest stays; so is it
/// below the floor while the census is refused. Fitted to a clipped line, the path gives way,
/// not the why.
#[test]
fn above_the_floor_or_switched_off_an_ended_sessions_build_is_any_other() {
    let tmp = Tmp::new("discover-off");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(4);
    let gone = scratch_build(&claude, &home, DEAD, "a");
    mkdirs(&home.join(".claude/sessions"));
    for (free, config) in [(100 * GIB, cfg(true)), (GIB, cfg(false))] {
        let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, free, config);
        let kinds: Vec<&str> = rep
            .rows
            .iter()
            .filter(|r| r.path == gone || r.path.starts_with(&gone))
            .map(|r| r.witness.kind())
            .collect();
        assert_eq!(kinds, ["stale-build-dir"], "{:#?}", rep.rows);
        assert!(rep.hogs.iter().any(|h| h.path == gone), "{:#?}", rep.hogs);
    }
    const WHY: &str = "process 4242 (/opt/x/bin/agent) could not be read, and may be a Claude Code";
    let unread = Roots {
        claude_unread: Some(WHY.to_owned()),
        ..roots_at(&tmp)
    };
    let (_, rep) = survey_at(&tmp, &unread, now, GIB, cfg(true));
    assert!(whole_row(&rep).is_none(), "{:#?}", rep.rows);
    assert!(
        rep.notes.iter().any(|n| n.contains(WHY)),
        "{:#?}",
        rep.notes
    );
    let hog = rep.hogs.iter().find(|h| h.path == gone).expect("a hog");
    assert!(hog.why.contains(WHY), "{}", hog.why);
    // What a `disk.low` detail line leaves the entry's second hog
    // (`DISK_LOW_HOG_CHARS` in the window).
    let fitted = hog.line_within(None, 158);
    assert!(
        fitted.contains(WHY) && fitted.chars().count() <= 158,
        "{fitted}"
    );
}

/// The largest hogs (pure): an x.py checkout's build directories are one line naming
/// slim_build.py, a line is what the rows would not free, a nested build directory counts in
/// the outer only, biggest first, at most [`super::super::MAX_HOGS`], paths as `~/…`.
#[test]
fn the_largest_hogs_are_what_no_row_frees_biggest_first() {
    let home = PathBuf::from("/h");
    let td = |path: &str, origin: Origin, gib: u64| TargetDir {
        path: PathBuf::from(path),
        marker: Some("CACHEDIR.TAG".to_owned()),
        holds_source: None,
        walk_cut_short: None,
        profiles: Vec::new(),
        device: Some(1),
        volume: None,
        probed_recent: false,
        origin,
        owned: Owned::NotOwned,
        total: Some((gib * GIB, false)),
        foreign: None,
        idle: None,
    };
    let xpy = Origin::Bootstrap {
        repo: home.join("trust"),
        slim: true,
    };
    let checkout = Origin::Checkout {
        root: home.join("trust"),
    };
    let mut s = Survey::new(1_000);
    s.home = Some(home.clone());
    s.targets = vec![
        td("/h/trust/build/aarch64/stage1-rustc", xpy.clone(), 36),
        td("/h/trust/build/aarch64/bootstrap-tools", xpy.clone(), 41),
        td("/h/trust/build/aarch64/stage1-tools", xpy, 17),
        td("/h/trust/crates/target.noindex", checkout.clone(), 49),
        td("/h/trust/crates/target.noindex/nested", checkout.clone(), 3),
        td("/h/a/target", checkout.clone(), 2),
        td("/h/b/target", checkout.clone(), 4),
        td("/h/c/target", checkout.clone(), 5),
        td("/h/d/target", checkout, 1),
    ];
    let rep = report(&s, Config::default());
    let hogs: Vec<(&Path, u64)> = rep
        .hogs
        .iter()
        .map(|h| (h.path.as_path(), h.bytes))
        .collect();
    assert_eq!(
        hogs,
        [
            ("/h/trust/build", 94),
            ("/h/trust/crates/target.noindex", 49),
            ("/h/c/target", 5),
            ("/h/b/target", 4),
            ("/h/a/target", 2),
        ]
        .map(|(path, gib)| (Path::new(path), gib * GIB))
    );
    assert!(
        rep.hogs[0]
            .line(rep.home.as_deref())
            .starts_with("$HOME/trust/build "),
        "a line reads `~/…`"
    );
    assert!(rep.to_json().contains("\"hogs\":["), "{}", rep.to_json());
    // One something else removed during the pass is no hog, and the next
    // moves up in its place.
    let gone = Path::new("/h/c/target");
    let left = crate::harness::disk::hogs_left(&s, &rep, &Default::default(), &|p| p != gone);
    let paths: Vec<&Path> = left.iter().map(|h| h.path.as_path()).collect();
    assert_eq!(
        paths,
        [
            "/h/trust/build",
            "/h/trust/crates/target.noindex",
            "/h/b/target",
            "/h/a/target",
            "/h/d/target"
        ]
        .map(Path::new)
    );
}

/// The hogs a tick hands the window name what the pass left, not what it planned: a whole row
/// refused at the removal, one gone only in part, and a profile a build locked just before
/// its row each stay a hog with why; a directory another process removed is no hog. Negative
/// control: a whole row that goes is no hog.
#[test]
fn the_hogs_a_tick_hands_the_window_name_what_the_pass_left() {
    use crate::harness::cli::{DiskLook, DiskTick, disk_tick};
    use crate::harness::disk::Refusal;
    use crate::harness::disk::target::total_bytes;
    use std::os::unix::fs::PermissionsExt as _;
    const CLEAN: &str = "0dead000-0000-4000-8000-000000000004";
    const VANISHED: &str = "0dead000-0000-4000-8000-000000000009";
    let tmp = Tmp::new("discover-hogs-left");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let state = mkdirs(&tmp.path().join("state"));
    let now = now_after_write(4);
    let refused = scratch_build(&claude, &home, DEAD, "a");
    let partial = scratch_build(&claude, &home, DEAD_TOO, "b");
    let clean = scratch_build(&claude, &home, CLEAN, "c");
    let vanished = scratch_build(&claude, &home, VANISHED, "d");
    let out = mkdirs(&partial.join("debug/build/ro-0123/out"));
    std::fs::write(out.join("kept.bin"), vec![9u8; 64 * 1024]).expect("kept");
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o555)).expect("read-only");
    mkdirs(&home.join(".claude/sessions")); // a registry naming nobody
    let live = canon(&lay_target(&home.join("live/target")));
    crate::harness::disk::tests::lay_profile(&live.join("release"));
    std::fs::write(
        live.join("release/incremental/lay-1x2y3z/s-abc-def/big.bin"),
        vec![5u8; 4 << 20],
    )
    .expect("big");
    last_used(&live.join("debug"), now - 2 * DAY);
    last_used(&live.join("release"), now - 2 * DAY);
    let mut held: Vec<Build> = Vec::new();
    // The running processes: one that cannot be read, seen from the first
    // build directory only.
    let processes = |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        Ok(PathScan {
            unreadable: if dirs.first() == Some(&refused) {
                vec![(4242, Some(PathBuf::from("/opt/x/bin/tests")))]
            } else {
                Vec::new()
            },
            ..PathScan::default()
        })
    };
    let roots = roots_at(&tmp);
    let table = aterm_sysprobe::volumes::table();
    let look = DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(GIB),
        targets: &[],
        discover: Some(&roots),
        transcripts: None,
        now,
        room: None,
        table: Some(&table),
        demand: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    };
    let mut real = real_with(now, processes);
    let mut remove = |row: &Row| {
        if row.path == live {
            held.push(Build::holds(&live.join("release/.cargo-lock")));
        }
        if row.path == vanished {
            std::fs::remove_dir_all(&vanished).expect("another process removes it");
        }
        real(row)
    };
    let tick = disk_tick(&look, cfg(true), &mut remove, &mut || Some(GIB));
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755)).expect("writable");
    let DiskTick::Reclaimed(done, hogs) = tick else {
        panic!("below the floor");
    };
    assert!(refused.join("debug/deps/liblay.rlib").exists(), "refused");
    assert!(out.join("kept.bin").exists(), "kept in part");
    assert!(!clean.exists(), "{:#?}", done.removed);
    assert!(live.join("release/incremental").exists(), "held");
    assert!(!live.join("debug/incremental").exists(), "idle");
    let bytes = |dir: &Path| hogs.iter().find(|h| h.path == dir).map(|h| h.bytes);
    assert_eq!(bytes(&vanished), None, "{hogs:#?}");
    assert!(
        done.denials.iter().any(|d| matches!(d,
            Refusal::RemoveFailed(p, why) if *p == vanished && why.contains("it is gone"))),
        "{:#?}",
        done.denials
    );
    assert_eq!(bytes(&live), Some(total_bytes(&live).0), "{hogs:#?}");
    let why = |dir: &Path| hogs.iter().find(|h| h.path == dir).map(|h| h.why.clone());
    assert!(
        why(&refused).is_some_and(
            |w| w.contains("kept: no longer a candidate") && w.contains("could not be read")
        ),
        "{hogs:#?}"
    );
    assert!(
        why(&partial).is_some_and(|w| w.contains("kept.bin")),
        "{hogs:#?}"
    );
    assert_eq!(why(&clean), None, "{hogs:#?}");
    let went = done.went();
    assert_eq!((went.whole, went.in_part), (1, 1), "{:#?}", done.removed);
}

// ---------------------------------------------------------------------------
// Ownership and process probes
// ---------------------------------------------------------------------------

/// An ended session's scratch folder: its temp directory (a task output
/// in it) and its transcript, both days old by `now`; no build directory.
/// Answers the session's temp directory (canonical).
fn session_folder(claude_tmp: &Path, home: &Path, session: &str) -> PathBuf {
    claude_root(claude_tmp);
    let sdir = claude_tmp.join("-proj").join(session);
    mkdirs(&sdir.join("scratchpad"));
    std::fs::write(mkdirs(&sdir.join("tasks")).join("t1.output"), b"done\n").expect("task");
    let projects = mkdirs(&home.join(".claude/projects/-proj"));
    std::fs::write(projects.join(format!("{session}.jsonl")), b"{}\n").expect("transcript");
    canon(&sdir)
}

/// The one whole row of `rep`, if it has one.
fn whole_row(rep: &Report) -> Option<Row> {
    rep.rows
        .iter()
        .find(|r| matches!(r.witness, Witness::OwnerGone { .. }))
        .cloned()
}

/// A `target*` link out of an ended session's scratchpad never makes what it names the
/// session's: found only through the scratchpad, the checkout's build directory is
/// [`Origin::Linked`] (no whole row, `deps/` kept), and a whole row for it is refused at the
/// removal since it is not inside the owner's folder.
#[test]
fn a_link_out_of_an_ended_sessions_scratchpad_never_gives_it_what_the_link_names() {
    let tmp = Tmp::new("discover-link-out");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(4);
    let deep = mkdirs(&home.join("src/group/work"));
    let noindex = canon(&lay_target(&deep.join("target.noindex")));
    symlink("target.noindex", deep.join("target")).expect("checkout link");
    let sdir = session_folder(&claude, &home, DEAD);
    let wt = mkdirs(&sdir.join("scratchpad/wt"));
    symlink(deep.join("target"), wt.join("target")).expect("link out");
    mkdirs(&home.join(".claude/sessions")); // a registry naming nobody

    let d = discover(&roots_at(&tmp), Budget::SHIPPED);
    let f = d
        .found
        .iter()
        .find(|f| f.path == noindex)
        .expect("found through the link");
    assert_eq!(
        f.origin,
        Origin::Linked {
            from: wt.join("target")
        }
    );
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    assert!(whole_row(&rep).is_none(), "{:#?}", rep.rows);
    let hog = rep.hogs.iter().find(|h| h.path == noindex).expect("a hog");
    assert!(hog.why.contains("reached through"), "{}", hog.why);
    apply_auto(&rep, None, &mut real(now), &mut || Some(GIB), &mut |_| true).expect("applied");
    assert!(noindex.join("debug/deps/liblay.rlib").exists());
    assert!(noindex.join("CACHEDIR.TAG").exists());

    // At the removal: a whole row made for it is refused.
    last_used(&noindex.join("debug"), now - 5 * DAY);
    let owner = Owner::ClaudeSession {
        session: DEAD.to_owned(),
        session_dir: sdir.clone(),
        claude_dirs: vec![home.join(".claude")],
        home: Some(home.clone()),
    };
    let forged = Row {
        class: Class::CargoTargets,
        path: noindex.clone(),
        bytes: 1,
        bytes_partial: false,
        witness: Witness::OwnerGone {
            marker: "CACHEDIR.TAG".to_owned(),
            evidence: "forged".to_owned(),
            owner,
            last_used: now - 5 * DAY,
            age_s: 5 * DAY,
            quiet_s: DAY,
            floor_gib: 10,
            until_gib: 11,
            device: super::device_of(&noindex).expect("device"),
        },
        removable: true,
        blocked: None,
    };
    let err = real(now)(&forged).expect_err("outside its owner's folder");
    assert!(
        err.to_string().contains("not inside its owner's folder"),
        "{err}"
    );
    assert!(noindex.join("debug/deps/liblay.rlib").exists());
}

/// A build directory a checkout also names is the checkout's, whichever walk met it first;
/// two owners at once make [`Origin::Linked`].
#[test]
fn a_build_directory_a_checkout_also_names_is_the_checkouts() {
    let tmp = Tmp::new("discover-shared");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(4);
    let sdir = session_folder(&claude, &home, DEAD);
    let shared = canon(&lay_target(&sdir.join("scratchpad/shared-target")));
    let live = mkdirs(&home.join("live"));
    symlink(&shared, live.join("target")).expect("checkout link into the scratchpad");
    mkdirs(&home.join(".claude/sessions"));

    let d = discover(&roots_at(&tmp), Budget::SHIPPED);
    let at: Vec<&Found> = d.found.iter().filter(|f| f.path == shared).collect();
    assert_eq!(at.len(), 1, "{:#?}", d.found);
    assert_eq!(at[0].origin, Origin::Checkout { root: canon(&live) });
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    assert!(whole_row(&rep).is_none(), "{:#?}", rep.rows);
    apply_auto(&rep, None, &mut real(now), &mut || Some(GIB), &mut |_| true).expect("applied");
    assert!(live.join("target/debug/deps/liblay.rlib").exists());

    let scratch = Origin::ClaudeScratch {
        session: DEAD.to_owned(),
        session_dir: sdir.clone(),
    };
    let checkout = Origin::Checkout { root: canon(&live) };
    assert_eq!(scratch.clone().merge(checkout.clone()), checkout);
    assert_eq!(checkout.clone().merge(scratch.clone()), checkout);
    assert_eq!(scratch.clone().merge(scratch.clone()), scratch);
    let other = Origin::ClaudeScratch {
        session: LIVE.to_owned(),
        session_dir: PathBuf::from("/claude-tmp/-proj/other"),
    };
    assert_eq!(
        scratch.merge(other),
        Origin::Linked {
            from: PathBuf::from("/claude-tmp/-proj/other")
        }
    );
}

/// Run `f` until it answers `Err` (a spawned child's working directory and
/// open files show at once; the hang detector allows a minute).
fn until_err(mut f: impl FnMut() -> Result<(), String>) -> Result<(), String> {
    within(MUST_HAPPEN, std::time::Duration::from_millis(20), || {
        f().err()
    })
    .map_or(Ok(()), Err)
}

/// A child working in an ended session's `deps/`, or holding a file of its folder open,
/// refuses the whole removal (cargo takes no lock while tests run); the shipped
/// [`remover`](crate::harness::disk::remover), the one judge here that reads every process,
/// names the child too. Negative control: with both exited, the row goes.
#[test]
fn a_process_running_from_an_ended_sessions_build_keeps_it() {
    let tmp = Tmp::new("discover-running");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(4);
    let gone = scratch_build(&claude, &home, DEAD, "a");
    mkdirs(&home.join(".claude/sessions"));
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    let row = whole_row(&rep).expect("a whole row");
    let Witness::OwnerGone { owner, .. } = &row.witness else {
        unreachable!("a whole row");
    };

    let mut tests = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("120")
        .current_dir(gone.join("debug/deps"))
        .spawn()
        .expect("a test binary's stand-in");
    let seen = until_err(|| unused(owner, &row.path, &among(vec![tests.id()])));
    let err = real_among(now, vec![tests.id()])(&row).expect_err("a process works in deps/");
    let shipped = remover(judge_at(now), crate::harness::disk::idle::Sessions::default)(&row)
        .expect_err("the product reads it too");
    let _ = tests.kill();
    let _ = tests.wait();
    assert!(seen.is_err_and(|why| why.contains("working directory")));
    assert!(err.to_string().contains("uses"), "{err}");
    let user = format!("process {} uses", tests.id());
    assert!(shipped.to_string().contains(&user), "{shipped}");
    assert!(gone.join("debug/deps/liblay.rlib").exists());

    let held = owner.dir().join("tasks/t1.output");
    let mut holder = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("exec 3<\"$1\"; exec /bin/sleep 120")
        .arg("sh")
        .arg(&held)
        .current_dir("/")
        .spawn()
        .expect("a holder");
    let seen = until_err(|| unused(owner, &row.path, &among(vec![holder.id()])));
    let err = real_among(now, vec![holder.id()])(&row)
        .expect_err("a process holds a file of the session's");
    let shipped = remover(judge_at(now), crate::harness::disk::idle::Sessions::default)(&row)
        .expect_err("the product reads it too");
    let _ = holder.kill();
    let _ = holder.wait();
    assert!(seen.is_err_and(|why| why.contains("open file")));
    assert!(err.to_string().contains("uses"), "{err}");
    let user = format!("process {} uses", holder.id());
    assert!(shipped.to_string().contains(&user), "{shipped}");
    assert!(gone.join("debug/deps/liblay.rlib").exists());

    // Negative control: both children are still in the universe, exited.
    let removed = real_among(now, vec![tests.id(), holder.id()])(&row).expect("nothing uses it");
    assert_eq!(removed.units, std::slice::from_ref(&gone));
    assert!(!gone.exists());
}

/// A process that cannot be read (not a platform service), or a failed process listing, keeps
/// a gone owner's build directory. Negative control: with nothing unreadable the row goes.
#[test]
fn a_process_that_cannot_be_read_keeps_a_gone_owners_build_directory() {
    let tmp = Tmp::new("discover-unreadable");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(4);
    let gone = scratch_build(&claude, &home, DEAD, "a");
    mkdirs(&home.join(".claude/sessions"));
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    let row = whole_row(&rep).expect("a whole row");
    let Witness::OwnerGone { owner, .. } = &row.witness else {
        unreachable!("a whole row");
    };
    let unreadable = |exe: Option<&str>| {
        let scan = PathScan {
            unreadable: vec![(4242, exe.map(PathBuf::from))],
            ..PathScan::default()
        };
        move |_: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> { Ok(scan.clone()) }
    };
    let python = "/opt/homebrew/bin/python3";
    let err = real_with(now, unreadable(Some(python)))(&row).expect_err("not ruled out");
    assert!(
        err.to_string()
            .contains(&format!("process 4242 ({python}) could not be read")),
        "{err}"
    );
    assert!(gone.join("debug/deps/liblay.rlib").exists());
    let why = unused(owner, &row.path, &unreadable(None)).expect_err("no executable read");
    assert!(why.contains("process 4242 could not be read"), "{why}");
    let failed = |_: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        Err(std::io::Error::other("no listing"))
    };
    let why = unused(owner, &row.path, &failed).expect_err("no listing");
    assert!(why.contains("could not be listed"), "{why}");
    assert_eq!(
        unused(owner, &row.path, &unreadable(Some("/usr/libexec/lsd"))),
        Ok(())
    );

    // Negative control: nothing unreadable, the same row goes.
    let removed = real(now)(&row).expect("nothing keeps it");
    assert_eq!(removed.units, std::slice::from_ref(&gone));
}

/// THE SEARCH STAYS ON THE MEASURED VOLUME AND OUT OF MOUNT POINTS (review
/// P5). With the fixtures' own device every build directory is found; with
/// a device none of them is on, none is — a root, `$HOME`'s children and the
/// temp root alike. A directory the mount table names is
/// never entered (nor `stat`ed): its build directory is not found, its
/// sibling's is; and a `target` link into a mount is never followed. And
/// the mount table lists the root.
#[test]
fn the_search_stays_on_the_measured_volume_and_out_of_mount_points() {
    let tmp = Tmp::new("discover-volume");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let a = canon(&lay_target(&home.join("a/target")));
    let b = canon(&lay_target(&home.join("b/target")));
    let scratch = scratch_build(&claude, &home, DEAD, "x");
    // A checkout whose `target` links onto another mount.
    let mounted_target = canon(&lay_target(&tmp.path().join("nas/proj/target")));
    symlink(&mounted_target, mkdirs(&home.join("c")).join("target")).expect("link to a mount");
    let dev = device_of(tmp.path()).expect("device");
    let found = |roots: &Roots| {
        let mut p: Vec<PathBuf> = discover(roots, Budget::SHIPPED)
            .found
            .into_iter()
            .map(|f| f.path)
            .collect();
        p.sort();
        p
    };
    let mut all = vec![a.clone(), b.clone(), scratch, mounted_target.clone()];
    all.sort();
    let on = Roots {
        cwds: vec![home.join("a")],
        device: Some(dev),
        ..roots_at(&tmp)
    };
    assert_eq!(found(&on), all);
    let elsewhere = Roots {
        device: Some(dev ^ 0x5a5a),
        ..on.clone()
    };
    assert!(found(&elsewhere).is_empty());
    let mounted = Roots {
        cwds: Vec::new(),
        mounts: vec![canon(&home).join("b"), canon(&tmp.path().join("nas"))],
        ..on
    };
    let got = found(&mounted);
    assert!(got.contains(&a) && !got.contains(&b), "{got:#?}");
    assert!(
        !got.contains(&mounted_target),
        "a link into a mount is never followed: {got:#?}"
    );
    assert!(super::mount_points().contains(&PathBuf::from("/")));
}

/// A working directory inside an x.py tree's `build/` never reaches stage0 or a sysroot: only
/// the bootstrap rule, from the worktree root, finds `stage1-rustc`.
#[test]
fn a_cwd_inside_an_xpy_build_tree_never_reaches_stage0_or_a_sysroot() {
    let tmp = Tmp::new("discover-xpy-cwd");
    let repo = mkdirs(&tmp.path().join("trust"));
    std::fs::write(repo.join("x.py"), b"#!/usr/bin/env python3\n").expect("x.py");
    let host = mkdirs(&repo.join("build/aarch64-apple-darwin"));
    let rustc = canon(&lay_target(&host.join("stage1-rustc")));
    for n in ["stage0-rustc", "stage1-std-sysroot", "stage2"] {
        lay_target(&host.join(n));
    }
    let only = |cwd: &Path| Roots {
        cwds: vec![cwd.to_path_buf()],
        ..Roots::default()
    };
    assert!(
        discover(&only(&host), Budget::SHIPPED).found.is_empty(),
        "no worktree root: nothing"
    );
    mkdirs(&repo.join(".git"));
    let d = discover(&only(&host), Budget::SHIPPED);
    let got: Vec<&PathBuf> = d.found.iter().map(|f| &f.path).collect();
    assert_eq!(got, [&rustc]);
    assert!(matches!(d.found[0].origin, Origin::Bootstrap { .. }));
}

/// The privacy-guarded folders are never walked from `$HOME`, but an agent working in one is
/// still searched from its own directory.
#[cfg(target_os = "macos")]
#[test]
fn the_privacy_guarded_folders_are_never_walked_from_home() {
    let tmp = Tmp::new("discover-tcc");
    let home = mkdirs(&tmp.path().join("home"));
    let mut planted = Vec::new();
    for dir in ["Desktop", "Documents", "Downloads"] {
        planted.push(canon(&lay_target(&home.join(dir).join("proj/target"))));
    }
    let d = discover(&roots_at(&tmp), Budget::SHIPPED);
    assert!(d.found.is_empty(), "{:#?}", d.found);
    let from_agent = Roots {
        cwds: vec![home.join("Documents/proj")],
        ..roots_at(&tmp)
    };
    let d = discover(&from_agent, Budget::SHIPPED);
    let got: Vec<&PathBuf> = d.found.iter().map(|f| &f.path).collect();
    assert_eq!(got, [&planted[1]]);
}

/// A temp root another user could write in, or a link, is skipped with a note. Negative
/// control: private again, its scratch build is found.
#[test]
fn a_temp_root_another_user_could_write_in_is_not_searched() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = Tmp::new("discover-tmp-perm");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let scratch = scratch_build(&claude, &home, DEAD, "x");
    let only_tmp = |root: PathBuf| Roots {
        claude_tmp: Some(root),
        ..Roots::default()
    };
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let d = discover(&only_tmp(claude.clone()), Budget::SHIPPED);
    assert!(d.found.is_empty(), "{:#?}", d.found);
    assert!(
        d.notes.iter().any(|n| n.contains("another user")),
        "{:?}",
        d.notes
    );
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let link = tmp.path().join("claude-link");
    symlink(&claude, &link).expect("link");
    let d = discover(&only_tmp(link), Budget::SHIPPED);
    assert!(d.found.is_empty(), "{:#?}", d.found);
    let d = discover(&only_tmp(claude), Budget::SHIPPED);
    let got: Vec<&PathBuf> = d.found.iter().map(|f| &f.path).collect();
    assert_eq!(got, [&scratch]);
}

/// A search cut short before `$HOME` is done reaches no owner's build directory: checkouts are
/// walked first. Negative control: with room, it is found.
#[test]
fn a_search_cut_short_in_home_reaches_no_owners_build_directory() {
    let tmp = Tmp::new("discover-cut-owner");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    for n in ["a", "b", "c"] {
        lay_target(&home.join(n).join("target"));
    }
    scratch_build(&claude, &home, DEAD, "x");
    let cut = discover(
        &roots_at(&tmp),
        Budget {
            found: 2,
            ..Budget::SHIPPED
        },
    );
    assert!(
        !cut.found.iter().any(|f| f.origin.ownable()),
        "{:#?}",
        cut.found
    );
    let whole = discover(&roots_at(&tmp), Budget::SHIPPED);
    assert!(whole.found.iter().any(|f| f.origin.ownable()));
}

/// The journal's intent row for a whole removal names the session and its folder.
#[test]
fn the_journal_names_whose_build_directory_went_whole() {
    let tmp = Tmp::new("discover-journal");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(4);
    let gone = scratch_build(&claude, &home, DEAD, "a");
    mkdirs(&home.join(".claude/sessions"));
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    let row = whole_row(&rep).expect("a whole row");
    let line = crate::harness::disk::step_row(now, "s", &Step::Removing(&row));
    let session_dir = gone
        .ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == DEAD))
        .expect("the session's folder");
    assert!(line.contains("\"owner\":"), "{line}");
    assert!(line.contains(DEAD), "{line}");
    assert!(line.contains(&session_dir.display().to_string()), "{line}");
}

/// One workspace's build directory in the fixture's Rust build cache
/// (`<cache><workspace>/.cargo-build`), its profile last used at `at`.
/// Answers the build directory (canonical).
fn cache_build(home: &Path, workspace: &Path, at: i64) -> PathBuf {
    let cache = atpkg::build_cache::cache_root(home);
    let dir = lay_target(&atpkg::build_cache::build_dir(&cache, workspace));
    last_used(&dir.join("debug"), at);
    canon(&dir)
}

/// The whole rows of `rep` by path.
fn whole_paths(rep: &Report) -> Vec<PathBuf> {
    rep.rows
        .iter()
        .filter(|r| matches!(r.witness, Witness::OwnerGone { .. }))
        .map(|r| r.path.clone())
        .collect()
}

/// BELOW THE FLOOR, A REMOVED WORKSPACE'S RUST BUILD CACHE GOES WHOLE AFTER
/// TEN MINUTES UNWRITTEN — the leftover of a deleted worktree that filled
/// the disk twice on 2026-10-16. Its owner is the workspace, gone when SEEN
/// gone (its folder's parent there without it) — and, the survey placing it
/// on the home volume with no mount point between, one gone WITH the folder
/// above it (a workspace nested in a removed checkout, 2026-10-17's seven
/// `codex-alab-*/codex-rs` mirrors). NEGATIVE CONTROLS, each kept byte for
/// byte: the workspace present (a `Cargo.toml`); one written into a minute
/// ago; one whose cargo lock a build holds; and the same survey above the
/// warning. A `.cargo-build` that is a link out of the cache is never found
/// at all, and its target survives.
#[test]
fn below_the_floor_a_removed_workspaces_build_cache_goes_whole() {
    let tmp = Tmp::new("discover-build-cache-gone");
    let home = mkdirs(&tmp.path().join("home"));
    let now = now_after_write(0);
    let quarter_hour = now - 15 * 60;
    let gone = cache_build(&home, &home.join("aterm-wt/removed"), quarter_hour);
    mkdirs(&home.join("aterm-wt"));
    let live_ws = mkdirs(&home.join("aterm-wt/live"));
    std::fs::write(live_ws.join("Cargo.toml"), "[workspace]\n").expect("manifest");
    let live = cache_build(&home, &live_ws, quarter_hour);
    let recent = cache_build(&home, &home.join("aterm-wt/just-built"), now - 60);
    let away = cache_build(&home, &home.join("away/ws"), quarter_hour);
    let locked = cache_build(&home, &home.join("aterm-wt/building"), quarter_hour);
    let outside = lay_target(&tmp.path().join("outside/.cargo-build"));
    let linked = atpkg::build_cache::build_dir(
        &atpkg::build_cache::cache_root(&home),
        &home.join("aterm-wt/linked"),
    );
    mkdirs(linked.parent().expect("mirror"));
    symlink(&outside, &linked).expect("link");

    // Above the warning: nothing goes whole.
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, 500 * GIB, cfg(true));
    assert!(whole_paths(&rep).is_empty(), "{:#?}", rep.rows);

    let build = Build::holds(&locked.join("debug/.cargo-lock"));
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    drop(build);
    assert_eq!(
        whole_paths(&rep),
        vec![gone.clone(), away.clone()],
        "{:#?}",
        rep.notes
    );
    let row = whole_row(&rep).expect("a whole row");
    let Witness::OwnerGone { owner, quiet_s, .. } = &row.witness else {
        unreachable!("a whole row");
    };
    assert_eq!(*quiet_s, crate::harness::disk::PRESSURE_MIN_IDLE_S);
    assert!(
        row.witness.describe().contains("workspace that is gone"),
        "{}",
        row.witness.describe()
    );
    assert!(matches!(owner, Owner::BuildCache { .. }));
    assert!(
        rep.notes
            .iter()
            .any(|n| n.contains(&recent.display().to_string()) && n.contains("quiet window")),
        "{:#?}",
        rep.notes
    );

    let mut journal = Vec::new();
    let done = apply_auto(&rep, None, &mut real(now), &mut || Some(GIB), &mut |step| {
        if let Step::Removed(r, _) = step {
            journal.push(r.path.clone());
        }
        true
    })
    .expect("below the floor");
    assert!(journal.contains(&gone), "{journal:?}");
    assert!(!gone.exists(), "the removed workspace's cache went whole");
    assert!(!away.exists(), "and the one nested in a removed checkout");
    assert!(done.freed_bytes >= 8192, "{}", done.freed_bytes);
    for kept in [&live, &recent, &locked] {
        assert!(
            kept.join("debug/deps/liblay.rlib").is_file(),
            "{} keeps its deps",
            kept.display()
        );
    }
    assert!(outside.join("debug/deps/liblay.rlib").is_file());
    assert!(
        std::fs::symlink_metadata(&linked)
            .expect("link")
            .file_type()
            .is_symlink()
    );
}

/// AT THE REMOVAL a removed workspace's build cache is judged again: a
/// worktree recreated since the survey (its `Cargo.toml` back) keeps it,
/// and so does a process working inside it. NEGATIVE CONTROL: with the
/// worktree removed again and the process gone, the same row goes.
#[test]
fn a_recreated_worktree_or_a_process_inside_keeps_a_build_cache() {
    let tmp = Tmp::new("discover-build-cache-recheck");
    let home = mkdirs(&tmp.path().join("home"));
    let now = now_after_write(0);
    let ws = home.join("wt");
    let dir = cache_build(&home, &ws, now - 15 * 60);
    let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, GIB, cfg(true));
    let row = whole_row(&rep).expect("a whole row");

    mkdirs(&ws);
    std::fs::write(ws.join("Cargo.toml"), "[workspace]\n").expect("manifest");
    let err = real(now)(&row).expect_err("the worktree is back");
    assert!(err.to_string().contains("is there"), "{err}");
    assert!(dir.join("debug/deps/liblay.rlib").is_file());
    std::fs::remove_dir_all(&ws).expect("removed again");

    let mut inside = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("120")
        .current_dir(dir.join("debug"))
        .spawn()
        .expect("a process inside");
    let Witness::OwnerGone { owner, .. } = &row.witness else {
        unreachable!("a whole row");
    };
    let seen = until_err(|| unused(owner, &row.path, &among(vec![inside.id()])));
    let err = real_among(now, vec![inside.id()])(&row).expect_err("a process works inside");
    let _ = inside.kill();
    let _ = inside.wait();
    assert!(seen.is_err_and(|why| why.contains("working directory")));
    assert!(err.to_string().contains("uses"), "{err}");
    assert!(dir.join("debug/deps/liblay.rlib").is_file());

    let removed = real_among(now, vec![inside.id()])(&row).expect("nothing keeps it now");
    assert_eq!(removed.units, std::slice::from_ref(&dir));
    assert!(!dir.exists());
}

/// This process's captured executable and working directory are checked
/// against owned real paths. A failed query or unresolved path cannot clear it.
#[test]
fn this_processs_own_executable_and_working_directory_are_users() {
    let tmp = aterm_tempfile::tempdir().expect("owned process-path fixture");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical fixture");
    let repo = root.join("repo");
    let exe = repo.join("target/release/aterm");
    let cwd = repo.join("src");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(exe.parent().expect("executable parent")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(&exe, b"captured executable").unwrap();
    let dirs = [repo.join("target"), repo.clone()];
    let found = own_use(
        &[("exe", Ok(exe.clone())), ("cwd", Ok(elsewhere.clone()))],
        &dirs,
    )
    .expect("complete captured paths");
    assert_eq!(found, Some(("exe", exe)));
    let found = own_use(
        &[("exe", Ok(elsewhere.clone())), ("cwd", Ok(cwd.clone()))],
        &dirs,
    )
    .expect("complete captured paths");
    assert_eq!(found, Some(("cwd", cwd)));
    assert!(
        own_use(
            &[
                ("exe", Ok(elsewhere.clone())),
                ("cwd", Ok(elsewhere.clone()))
            ],
            &dirs,
        )
        .expect("two resolved outside paths")
        .is_none()
    );
    let denied = || std::io::Error::new(std::io::ErrorKind::PermissionDenied, "captured denial");
    let exe_unknown = own_use(
        &[("exe", Err(denied())), ("cwd", Ok(elsewhere.clone()))],
        &dirs,
    );
    let cwd_unknown = own_use(
        &[("exe", Ok(elsewhere.clone())), ("cwd", Err(denied()))],
        &dirs,
    );
    let unresolved = own_use(
        &[("exe", Ok(elsewhere)), ("cwd", Ok(root.join("vanished")))],
        &dirs,
    );
    assert!(
        exe_unknown.is_err() && cwd_unknown.is_err() && unresolved.is_err(),
        "OWN_PROCESS_UNKNOWN_MUST_NOT_CLEAR: exe={exe_unknown:?}, cwd={cwd_unknown:?}, unresolved={unresolved:?}"
    );
}

/// `discover` never enters `~/Library/Containers`, `~/Library/Group Containers`,
/// `~/Library/CloudStorage` or `~/Library/Mobile Documents` — whether handed as
/// `CARGO_TARGET_DIR`, as a session `cwd`, as a `~/Library` child during the
/// `$HOME` walk, as a `target` symlink inside a checkout, or as a linked
/// worktree — while a target under `~/Library/Caches` (outside the fence) IS
/// found.
#[test]
fn other_apps_data_is_never_entered_from_any_root() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-discover-app-data-")
        .tempdir()
        .expect("owned discovery scratch");
    let home = canon(&mkdirs(&tmp.path().join("home")));
    let container_target = home.join("Library/Containers/com.other.App/Data/project/target");
    let group_target = home.join("Library/Group Containers/group.other/project/target");
    let cloud_target = home.join("Library/CloudStorage/Provider/project/target");
    let mobile_target = home.join("Library/Mobile Documents/com~apple~CloudDocs/project/target");
    let cache_target = home.join("Library/Caches/tool-build/target");
    for t in [
        &container_target,
        &group_target,
        &cloud_target,
        &mobile_target,
        &cache_target,
    ] {
        lay_target(t);
    }
    let repo = mkdirs(&home.join("repo"));
    #[cfg(unix)]
    {
        // A checkout whose `target` symlink points into `~/Library/Containers`
        // must not follow the link.
        symlink(&container_target, repo.join("target")).unwrap();
        // A checkout whose `.git/worktrees/wt/gitdir` is a symlink into
        // `~/Library/Containers` must not walk the container worktree either.
        let wt_meta = mkdirs(&repo.join(".git/worktrees/wt"));
        let wt_in_container = mkdirs(&container_target.parent().unwrap().join(".git"));
        let wt_link = home.join("wt-link");
        symlink(&wt_in_container, &wt_link).unwrap();
        std::fs::write(wt_meta.join("gitdir"), format!("{}\n", wt_link.display())).unwrap();
        // A real final cache directory behind an ancestor alias is still fenced.
        // The old final-component lstat alone admitted this App Data walk.
        let hidden_cache_parent = home.join("Library/Containers/com.other.App/Data/build-cache");
        let hidden_cache = hidden_cache_parent.join("cargo-build.noindex");
        lay_target(&hidden_cache.join("fixture/.cargo-build"));
        symlink(&hidden_cache_parent, home.join("Library/Caches/aterm")).unwrap();
    }

    let roots = Roots {
        home: Some(home.clone()),
        cwds: vec![
            container_target.parent().unwrap().to_path_buf(),
            group_target.parent().unwrap().to_path_buf(),
            cloud_target.parent().unwrap().to_path_buf(),
            mobile_target.parent().unwrap().to_path_buf(),
            cache_target.parent().unwrap().to_path_buf(),
            repo,
        ],
        agent_target_dirs: vec![
            container_target.clone(),
            group_target.clone(),
            cloud_target.clone(),
            mobile_target.clone(),
        ],
        claude_dirs: vec![home.join(".claude")],
        claude_tmp: Some(tmp.path().join("claude-tmp")),
        ..Roots::default()
    };
    let got = discover(&roots, Budget::SHIPPED);
    let paths: Vec<&Path> = got.found.iter().map(|f| f.path.as_path()).collect();
    assert_eq!(
        paths,
        [cache_target.as_path()],
        "only the control target under ~/Library/Caches may be discovered; got {paths:?}"
    );

    #[cfg(unix)]
    {
        let cache_parent = home.join("Library/Caches/aterm");
        std::fs::remove_file(&cache_parent).expect("owned cache alias");
        let cache_root = atpkg::build_cache::cache_root(&home);
        let normal_cache = lay_target(&cache_root.join("fixture/.cargo-build"));
        let got = discover(&roots, Budget::SHIPPED);
        let paths: Vec<&Path> = got.found.iter().map(|f| f.path.as_path()).collect();
        assert_eq!(
            paths,
            [cache_target.as_path(), normal_cache.as_path()],
            "an ordinary unfenced cache is still discovered"
        );
    }
}

/// THE CENSUS'S WORD ON A CLAUDE CODE'S LOGIN ([`Exec::stored_login`], the
/// environment's half of the host's verdict, which the account stats read as
/// `api-key-seen`): an exec environment with an API key, Bedrock's switch or
/// a route of its own is on no stored login. NEGATIVE CONTROLS: one with
/// none of them is; one that reads empty was not read, and is not.
#[test]
fn the_census_reads_whether_a_claude_code_is_on_its_stored_login() {
    let stored = |env: &[&str]| {
        Exec::of(atpkg::caller_shell::ProcArgs {
            exec_path: "/u/bin/claude".to_owned(),
            argv: vec!["claude".to_owned()],
            env: env.iter().map(|kv| (*kv).to_owned()).collect(),
        })
        .expect("an exec record")
        .stored_login
    };
    assert!(stored(&["HOME=/u", "TERM=xterm"]));
    for env in [
        ["HOME=/u", "ANTHROPIC_API_KEY=k"],
        ["HOME=/u", "CLAUDE_CODE_USE_BEDROCK=1"],
        ["HOME=/u", "ANTHROPIC_BASE_URL=https://gateway.example.com"],
    ] {
        assert!(!stored(&env), "{env:?}");
    }
    assert!(!stored(&[]), "not read");
}
