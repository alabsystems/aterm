// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for finding the build directories and for the build directory
//! whose owner is gone.
//!
//! Every tree lives in its own scratch directory ([`Tmp`]): a `home/` that
//! stands in for `$HOME` (its checkouts, a Claude Code registry and
//! transcripts under `home/.claude`), a Claude Code temp root, a cells cache.
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
    Trigger, Witness, apply_auto, remover, remover_among, report, scan,
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

/// A Claude Code session's scratch build: `<tmp>/<project>/<session>/scratchpad/<sub>/target`,
/// with a task output beside it and its transcript under `home`. Answers the
/// build directory (canonical).
fn scratch_build(claude_tmp: &Path, home: &Path, session: &str, sub: &str) -> PathBuf {
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
        cells: Some(tmp.path().join("cells")),
        device: None,
        mounts: Vec::new(),
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
                Origin::Bootstrap { .. } => "bootstrap",
                Origin::ClaudeScratch { .. } => "scratch",
                Origin::Cell { .. } => "cell",
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
/// Claude Code session's scratch build three levels down; a cells cache.
/// NEGATIVE CONTROLS: a build directory three levels down a checkout, one in
/// a hidden directory, one under `~/Library`, one in `$HOME`'s own hidden
/// children (`~/.cargo` carries a `CACHEDIR.TAG` too), and one deeper than
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
    // A cells cache. Its marker holds the checkout's path byte for byte, as
    // `xtask gate cells` writes it: a live checkout whose folder name ends in
    // a space is read as that folder (a trimmed read named the missing `live`
    // beside it, seen gone, and the cache went whole). A marker ending in a
    // line break (no marker the gate writes) names no checkout.
    let live = mkdirs(&t.join("live "));
    std::fs::write(live.join("Cargo.toml"), b"[workspace]\n").expect("manifest");
    let cell = lay_target(&t.join("cells/wt-0123/linux"));
    std::fs::write(
        t.join("cells/wt-0123/.checkout"),
        canon(&live).as_os_str().as_encoded_bytes(),
    )
    .expect("marker");
    let lined = lay_target(&t.join("cells/wt-0456/linux"));
    std::fs::write(
        t.join("cells/wt-0456/.checkout"),
        format!("{}\n", canon(&live).display()),
    )
    .expect("marker");

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
        (canon(&cell), "cell"),
        (canon(&lined), "cell"),
    ];
    want.sort();
    assert_eq!(got, want);
    // The scratch build carries its session, and each cell its checkout.
    for f in &d.found {
        match &f.origin {
            Origin::ClaudeScratch { session, .. } => assert_eq!(session, DEAD),
            Origin::Cell { checkout, .. } if f.path == canon(&cell) => {
                assert_eq!(checkout.as_deref(), Some(canon(&live).as_path()));
            }
            Origin::Cell { checkout, .. } => assert_eq!(*checkout, None),
            _ => {}
        }
    }
}

/// A BUDGET SPENT STOPS THE SEARCH AND SAYS SO, keeping what it found.
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

/// X.PY'S BOOTSTRAP TREES GIVE THEIR CARGO BUILD DIRECTORIES — AND NEVER A
/// SYSROOT. Under `build/<host>/` only `stage<N>-<kind>` (N of 1 or more)
/// and `bootstrap-tools`, each with a signed `CACHEDIR.TAG`, are build
/// directories. Planted with a `CACHEDIR.TAG` all the same, `stage2` (a
/// sysroot), `stage0`, `stage0-rustc`, `llvm`, `stage2-tools-bin` and
/// `stage1-sysroot` are NOT found; `stage1-std` without a signed tag is not;
/// `build/host -> <triple>` (a link) is not followed into a second copy; and
/// `build/bootstrap` (x.py's own build) is left to nobody — the marker walk
/// does not enter an x.py checkout's `build/`.
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
/// refuse. A cells cache is gone only when its checkout is SEEN to be — its
/// folder there without a `Cargo.toml`, or the folder that held it there
/// without it — never when the checkout cannot be read, is away with the
/// folder above it, sits at a volume's root, or (macOS) lies in a
/// privacy-guarded folder, and the guarded places are never looked at.
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
    // A session running (this process) under `alt/.claude`, which no tab
    // supervises and nobody named, whose `projects/` IS `~/.claude`'s,
    // linked: its transcript is in a known directory, its registry is not.
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
    // WHO MAY HOLD THE LINKED SESSION, from the kernel: its Claude Code runs
    // (the census's word), started as this account — so the census names
    // `~/.claude`, not `alt/.claude`, where its settings moved its registry.
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
    // Neither the system's own executables (macOS hides their environment)
    // nor a process that exited may be one; a process that may be a Claude
    // Code nobody can read makes no session ended — its exec record hidden
    // (its executable named, or unlinked since its exec and named by no
    // path: it still runs).
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
    // FOR REACH, each process's own directory: aterm's identity's (in
    // `~/Library` on macOS) is read; another place of `~/Library`, one that
    // climbs out of aterm's by `..`, or one not absolute is not — and none
    // refuses the census.
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
    assert_eq!(reach.map(|r| r.dirs), Ok(vec![identity]));
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
    // Cells.
    let checkout = mkdirs(&tmp.path().join("checkout"));
    let mut cells = Owners::new(Some(home.clone()), Vec::new(), Vec::new(), None, now, DAY);
    let key = tmp.path().to_path_buf();
    let cell = |c: &Path| Origin::Cell {
        checkout: Some(c.to_path_buf()),
        key: key.clone(),
    };
    assert!(matches!(cells.of(&cell(&checkout)), Owned::Gone { .. }));
    // Removed, the folder that held it there: gone.
    assert!(matches!(
        cells.of(&cell(&tmp.path().join("removed"))),
        Owned::Gone { .. }
    ));
    // NOT SEEN, so not gone: a checkout whose folder and the one that held
    // it are both missing (a volume not mounted now); one at a volume's root
    // (the folder above it, where volumes mount, is there); one that is there
    // but cannot be read; and (macOS) one in a privacy-guarded folder, there
    // without a Cargo.toml, which is never looked into.
    let volumes = if cfg!(target_os = "macos") {
        "/Volumes"
    } else {
        "/media"
    };
    let volume = PathBuf::from(format!(
        "{volumes}/aterm-not-mounted-{}",
        std::process::id()
    ));
    let mut guarded = vec![volume.join("repo"), volume];
    if cfg!(target_os = "macos") {
        guarded.push(mkdirs(&home.join("Documents/repo")));
    }
    // The guarded places are not even looked at.
    for c in &guarded {
        let not_looked = super::checkout::gone_by(c, Some(&home), &mut |p| {
            panic!("{} looked at {}", c.display(), p.display())
        });
        assert!(not_looked.is_err(), "{}", c.display());
    }
    // The walled checkout: its folder is there, its Cargo.toml unreadable.
    let walled = mkdirs(&tmp.path().join("walled"));
    std::fs::write(walled.join("Cargo.toml"), b"[workspace]\n").expect("manifest");
    let mode = |m: u32| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&walled, std::fs::Permissions::from_mode(m)).expect("chmod");
    };
    mode(0o000);
    let refused = std::fs::symlink_metadata(walled.join("Cargo.toml"));
    let mut unseen = vec![tmp.path().join("away/repo"), walled.clone()];
    unseen.extend(guarded);
    let verdicts: Vec<(PathBuf, Owned)> = unseen
        .iter()
        .map(|c| (c.clone(), cells.of(&cell(c))))
        .collect();
    mode(0o755);
    assert!(
        refused.is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied),
        "the walled folder must refuse this reader"
    );
    let gone: Vec<&PathBuf> = verdicts
        .iter()
        .filter(|(_, v)| !matches!(v, Owned::Live(_)))
        .map(|(c, _)| c)
        .collect();
    assert!(gone.is_empty(), "called gone: {gone:#?}");
    std::fs::write(checkout.join("Cargo.toml"), b"[workspace]\n").expect("manifest");
    assert!(matches!(cells.of(&cell(&checkout)), Owned::Live(_)));
    assert!(matches!(
        cells.of(&Origin::Cell {
            checkout: None,
            key: key.clone()
        }),
        Owned::Live(_)
    ));
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
    }
}

/// The real remover at `now`, among the running processes `processes` reads;
/// none of them names a Claude Code directory the survey did not read.
fn real_with(
    now: i64,
    processes: impl Fn(&[PathBuf], &[u32]) -> std::io::Result<PathScan>,
) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    remover_among(judge_at(now), processes, |_| Ok(Running::default()))
}

/// The real remover at `now`, among the processes `pids` (the test's own).
fn real_among(now: i64, pids: Vec<u32>) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    real_with(now, among(pids))
}

/// The real remover at `now`, where no process of the test's runs.
fn real(now: i64) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    real_among(now, Vec::new())
}

/// BELOW THE FLOOR, AN ENDED SESSION'S BUILD GOES WHOLE — AFTER THE IDLE
/// CACHES, BEFORE ANY LIVE CHECKOUT'S, THE LONGEST UNWRITTEN FIRST, AND THE
/// PASS STOPS WHEN FREE SPACE IS BACK. Four build directories: a checkout's
/// idle profile (two days unbuilt), two ended sessions' scratch builds (five
/// and three days unwritten), and a live checkout's profile built three
/// hours ago. The report lists them in the order the pass takes them: the
/// idle cache (taken whole, no re-measure), the ended sessions' build
/// directories oldest first, then the least recently used profile. Free
/// space measured again is short before the first whole directory and back
/// at the target before the second: the pass stops there. The first ended
/// session's build directory is GONE, root and all; the idle checkout keeps
/// everything but its `incremental/`; the second ended session's build and
/// the live checkout's cache are untouched. The journal says it as it
/// happened: intent, outcome, stop.
#[test]
fn below_the_floor_an_ended_sessions_build_goes_whole_in_lru_order_after_the_idle_caches() {
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
    let order: Vec<(&str, PathBuf)> = rep
        .rows
        .iter()
        .filter(|r| r.class == Class::CargoTargets)
        .map(|r| (r.witness.kind(), r.path.clone()))
        .collect();
    assert_eq!(
        order,
        [
            ("stale-build-dir", idle.clone()),
            ("owner-gone", old.clone()),
            ("owner-gone", newer.clone()),
            ("least-recently-used", live.join("debug")),
        ],
        "{:#?}",
        rep.notes
    );
    assert!(
        rep.rows[1].witness.describe().contains("ended"),
        "{}",
        rep.rows[1].witness.describe()
    );
    // Whole rows are pressure rows: counted under pressure, not whole.
    assert!(rep.reclaimable_under_pressure() > 0);

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
            format!("removing {}", idle.display()),
            format!("removed {}", idle.display()),
            format!("removing {}", old.display()),
            format!("removed {}", old.display()),
            "stopped measured".to_owned(),
        ]
    );
    assert_eq!(reads, 2, "measured before each pressure row it reached");
    let stop = done.stopped.expect("stopped");
    assert_eq!((stop.by, stop.left), (StopBy::Measured, 2));
    assert!(
        !old.exists(),
        "the ended session's build directory is gone whole"
    );
    assert!(
        old.parent().expect("parent").exists(),
        "its scratchpad stays"
    );
    assert!(!idle.join("debug/incremental").exists());
    assert!(
        idle.join("debug/deps/liblay.rlib").exists(),
        "deps/ is never an idle row's"
    );
    assert!(newer.join("debug/incremental").exists() && newer.exists());
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

    // AT THE REMOVAL. A clean survey of an ended session's build...
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
    // NEGATIVE CONTROL: with the record gone again, the same row goes.
    std::fs::remove_file(home.join(format!(".claude/sessions/{}.json", std::process::id())))
        .expect("unregister");
    let removed = real(now)(&row).expect("the owner is gone again");
    assert_eq!(removed.units, std::slice::from_ref(&gone));
    assert!(!gone.exists());
}

/// ABOVE THE FLOOR, OR SWITCHED OFF, AN ENDED SESSION'S BUILD IS ANY OTHER:
/// no whole row — its idle profile's `incremental/` is the row — and the
/// largest-hogs list says why the rest stays and when it would go. So it is
/// below the floor while a process that may be a Claude Code cannot be read
/// (the census refused): KEPT, and its hog line names that process, as the
/// report's notes (`aterm harness disk`) do — and fitted to a line clipped
/// at its end (the window's `disk.low`), the path gives way, not the why.
#[test]
fn above_the_floor_or_switched_off_an_ended_sessions_build_is_any_other() {
    let tmp = Tmp::new("discover-off");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let now = now_after_write(4);
    let gone = scratch_build(&claude, &home, DEAD, "a");
    mkdirs(&home.join(".claude/sessions"));
    for (free, config, says) in [
        (100 * GIB, cfg(true), "goes whole below the 10 GiB floor"),
        (GIB, cfg(false), "dead_build_dirs` is off"),
    ] {
        let (_, rep) = survey_at(&tmp, &roots_at(&tmp), now, free, config);
        let kinds: Vec<&str> = rep
            .rows
            .iter()
            .filter(|r| r.path == gone || r.path.starts_with(&gone))
            .map(|r| r.witness.kind())
            .collect();
        assert_eq!(kinds, ["stale-build-dir"], "{:#?}", rep.rows);
        let hog = rep.hogs.iter().find(|h| h.path == gone).expect("a hog");
        assert!(hog.why.contains(says), "{}", hog.why);
        assert!(hog.why.contains("ended Claude Code session"), "{}", hog.why);
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

/// THE LARGEST HOGS, from a survey literal (pure): x.py's build directories
/// are ONE line under their checkout's `build/` naming slim_build.py; a
/// build directory's line is what its rows would NOT free; a build
/// directory nested in another is counted in the outer one only; biggest
/// first, at most [`super::super::MAX_HOGS`]; a line reads `~/…`.
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
        probed_recent: false,
        origin,
        owned: Owned::NotOwned,
        total: Some((gib * GIB, false)),
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
    let lines: Vec<String> = rep
        .hogs
        .iter()
        .map(|h| h.line(rep.home.as_deref()))
        .collect();
    assert_eq!(
        lines,
        [
            "$HOME/trust/build 94.0 GiB (x.py bootstrap; run scripts/slim_build.py)",
            "$HOME/trust/crates/target.noindex 49.0 GiB (cargo build output: deps/ and build/ are `targo clean`'s)",
            "~/c/target 5.0 GiB (cargo build output: deps/ and build/ are `targo clean`'s)",
            "~/b/target 4.0 GiB (cargo build output: deps/ and build/ are `targo clean`'s)",
            "~/a/target 2.0 GiB (cargo build output: deps/ and build/ are `targo clean`'s)",
        ]
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

/// THE HOGS A TICK HANDS THE WINDOW NAME WHAT THE PASS LEFT, not what it
/// planned (the window's `disk.low` line, "Largest it cannot reclaim").
/// Below the floor four ended sessions' scratch builds are whole rows, and
/// the report counts each as freed. At the removal one is refused — a
/// process that cannot be read is seen from it, nothing is deleted — and
/// one goes only in part (a read-only build-script output keeps a file):
/// the hogs name both, each with why it stayed, and the pass's tally says
/// one went whole and one in part. A live checkout's two idle profiles are
/// one row, and a build takes `release/`'s lock just before it runs: only
/// `debug/`'s cache goes, and its hog is what the directory still holds. The
/// fourth session's build directory another process removes just before its
/// row runs: its denial says it is gone, and no hog names it. NEGATIVE
/// CONTROL: the third goes whole and is no hog.
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
    let look = DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(GIB),
        targets: &[],
        discover: Some(&roots),
        transcripts: None,
        now,
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
// The review of 2026-10-01: each probe, kept
// ---------------------------------------------------------------------------

/// An ended session's scratch folder: its temp directory (a task output
/// in it) and its transcript, both days old by `now`; no build directory.
/// Answers the session's temp directory (canonical).
fn session_folder(claude_tmp: &Path, home: &Path, session: &str) -> PathBuf {
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

/// A `target*` LINK OUT OF AN ENDED SESSION'S SCRATCHPAD NEVER MAKES WHAT IT
/// NAMES THE SESSION'S (review P1, HIGH). A checkout three levels under
/// `$HOME` (beyond the `$HOME` walk) has `target -> target.noindex`; an
/// ended session's scratch worktree links its own `target` to that link.
/// Found only through the scratchpad, the checkout's build directory is
/// [`Origin::Linked`] — no owner, no whole row, and the pass takes only its
/// idle `incremental/`, never `deps/`. AND AT THE REMOVAL: a whole row for
/// it (what the survey made before this fix) is refused, nothing deleted —
/// the directory is not inside its owner's folder.
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

    // AT THE REMOVAL: the row the old survey made is refused.
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

/// A BUILD DIRECTORY A CHECKOUT ALSO NAMES IS THE CHECKOUT'S, whichever walk
/// met it first (review P3). `$HOME/live/target` links into an ended
/// session's scratchpad: found by both walks, it is ONE directory, the
/// checkout's — no whole row, `deps/` kept, and the checkout's link still
/// resolves. The merge rule both ways, and two owners at once make
/// [`Origin::Linked`].
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
    let other = Origin::Cell {
        checkout: None,
        key: PathBuf::from("/cells/k"),
    };
    assert_eq!(
        scratch.merge(other),
        Origin::Linked {
            from: PathBuf::from("/cells/k")
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

/// A PROCESS RUNNING FROM AN ENDED SESSION'S BUILD KEEPS IT (review P2).
/// Cargo takes no lock while tests run, so a session nobody names can still
/// have something running: a child working in `target/debug/deps` refuses
/// the whole removal at the remover, nothing deleted; so does one holding a
/// file of the session's folder open from elsewhere. NEGATIVE CONTROL: with
/// both gone, the same row goes. The one test here that reads REAL
/// processes: each child it starts, read by the product's reader, and no
/// other process of the machine's — and, while each runs, THE PRODUCT'S OWN
/// [`remover`](crate::harness::disk::remover), which reads every process of
/// the uid: it names the child. That verdict no other process can turn
/// ([`unused`] names a user before it weighs an unreadable process, and only
/// this test's children are in its fresh directories), and it is what ties
/// the shipped wiring to the processes actually running — a remover that
/// read fewer would let the child's build go.
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

    let mut tests = std::process::Command::new("/bin/sleep")
        .arg("120")
        .current_dir(gone.join("debug/deps"))
        .spawn()
        .expect("a test binary's stand-in");
    let seen = until_err(|| unused(owner, &row.path, &among(vec![tests.id()])));
    let err = real_among(now, vec![tests.id()])(&row).expect_err("a process works in deps/");
    let shipped = remover(judge_at(now))(&row).expect_err("the product reads it too");
    let _ = tests.kill();
    let _ = tests.wait();
    assert!(seen.is_err_and(|why| why.contains("working directory")));
    assert!(err.to_string().contains("uses"), "{err}");
    let user = format!("process {} uses", tests.id());
    assert!(shipped.to_string().contains(&user), "{shipped}");
    assert!(gone.join("debug/deps/liblay.rlib").exists());

    let held = owner.dir().join("tasks/t1.output");
    let mut holder = std::process::Command::new("/bin/sh")
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
    let shipped = remover(judge_at(now))(&row).expect_err("the product reads it too");
    let _ = holder.kill();
    let _ = holder.wait();
    assert!(seen.is_err_and(|why| why.contains("open file")));
    assert!(err.to_string().contains("uses"), "{err}");
    let user = format!("process {} uses", holder.id());
    assert!(shipped.to_string().contains(&user), "{shipped}");
    assert!(gone.join("debug/deps/liblay.rlib").exists());

    // NEGATIVE CONTROL: nothing runs from it now — both children are still
    // in the universe, exited.
    let removed = real_among(now, vec![tests.id(), holder.id()])(&row).expect("nothing uses it");
    assert_eq!(removed.units, std::slice::from_ref(&gone));
    assert!(!gone.exists());
}

/// A PROCESS THAT CANNOT BE READ KEEPS A GONE OWNER'S BUILD DIRECTORY — the
/// product's caution, judged on a scan this test writes: an unreadable
/// process, its executable known or not, refuses the whole removal, nothing
/// deleted, and so does a process listing that fails; a platform service's
/// (an executable on the system's own tree) does not. NEGATIVE CONTROL:
/// with nothing unreadable the same row goes.
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

    // NEGATIVE CONTROL: nothing unreadable — the same row goes.
    let removed = real(now)(&row).expect("nothing keeps it");
    assert_eq!(removed.units, std::slice::from_ref(&gone));
}

/// THE SEARCH STAYS ON THE MEASURED VOLUME AND OUT OF MOUNT POINTS (review
/// P5). With the fixtures' own device every build directory is found; with
/// a device none of them is on, none is — a root, `$HOME`'s children, the
/// temp root and the cells alike. A directory the mount table names is
/// never entered (nor `stat`ed): its build directory is not found, its
/// sibling's is; and a `target` link into a mount is never followed. And
/// the mount table's escapes decode.
#[test]
fn the_search_stays_on_the_measured_volume_and_out_of_mount_points() {
    let tmp = Tmp::new("discover-volume");
    let home = mkdirs(&tmp.path().join("home"));
    let claude = tmp.path().join("claude-tmp");
    let a = canon(&lay_target(&home.join("a/target")));
    let b = canon(&lay_target(&home.join("b/target")));
    let scratch = scratch_build(&claude, &home, DEAD, "x");
    let cell = canon(&lay_target(&tmp.path().join("cells/k/linux")));
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
    let mut all = vec![a.clone(), b.clone(), scratch, cell, mounted_target.clone()];
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
    assert_eq!(
        super::unescape_mount("/Volumes/My\\040Disk"),
        PathBuf::from("/Volumes/My Disk")
    );
    assert!(super::mount_points().contains(&PathBuf::from("/")));
}

/// A WORKING DIRECTORY INSIDE AN X.PY TREE'S `build/` NEVER REACHES STAGE0
/// OR A SYSROOT (review P4). From `build/<host>/` the plain walk finds
/// nothing there; the bootstrap rule, from the checkout its worktree root
/// is, finds only `stage1-rustc`.
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

/// THE PRIVACY-GUARDED FOLDERS ARE NEVER WALKED FROM `$HOME` (review 11):
/// a checkout under `~/Documents` (or `~/Desktop`, `~/Downloads`) is not
/// searched by the window — a walk there can raise a privacy prompt — but
/// an agent working in it is still searched from its own directory.
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

/// A TEMP ROOT ANOTHER USER COULD WRITE IN IS NOT SEARCHED (review 14): on a
/// shared `/tmp` another local user could make `claude-<uid>` first and
/// plant links. Writable by others, or a link, it is skipped with a note;
/// NEGATIVE CONTROL, private again, its scratch build is found.
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

/// A SEARCH CUT SHORT BEFORE `$HOME` IS DONE REACHES NO OWNER'S BUILD
/// DIRECTORY (review 10): the checkouts are walked first, so an ended
/// session's build is never judged while a checkout that may name it went
/// unread. NEGATIVE CONTROL: with room, it is found.
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

/// THE JOURNAL EXPLAINS A WHOLE REMOVAL (review 12): the intent row of an
/// ended session's build directory names the session and its folder.
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
