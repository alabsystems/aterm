// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness probe repo` (HX-22): a harness holding the session's proven claim reads what the
//! window harness's own Land reads of the session's repository, with the same reader; any other
//! line reads nothing.

use std::cell::Cell;
use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use aterm_agent::harness::relaunch::Snapshot;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use aterm_agent::harness::repo_work::RepoRead;
use aterm_agent::harness::repo_work::Used;

use super::*;
use crate::session_timeline::{claim_supervisor_as, claim_supervisor_scoped, release_supervisor};

/// The connection the claim is bound to.
const CONN: u64 = 47;

const PROBE: &str = "probe repo";

fn no_proof(_: &str) -> Option<Proven> {
    None
}

/// A codex-alab holder on the stand-in process, which runs.
fn me() -> String {
    format!("codex-alab@{}", crate::self_claim::stand_in::pid())
}

/// `git <args>` in `dir`, hermetic (no system or global configuration, a fixed author), within
/// the hang detector.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn git(dir: &Path, args: &[&str]) {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
            .args([
                "-c",
                "user.name=Andrew Yates",
                "-c",
                "user.email=a@example.invalid",
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null"),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("git {args:?} within the hang detector: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository at `<root>/work` with one commit pushed to a bare `origin` at
/// `<root>/remote.git`'s `main`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn pushed_repo(root: &Path) -> PathBuf {
    let work = root.join("work");
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::create_dir_all(&bare).expect("bare");
    git(&bare, &["init", "-q", "--bare", "."]);
    git(&work, &["init", "-q", "."]);
    std::fs::write(work.join("a.txt"), "one\n").expect("a.txt");
    git(&work, &["add", "a.txt"]);
    git(&work, &["commit", "-q", "-m", "one"]);
    git(
        &work,
        &["remote", "add", "origin", &bare.display().to_string()],
    );
    git(&work, &["push", "-q", "-u", "origin", "HEAD:main"]);
    work
}

/// One session, and a scratch home, state root and repository folder.
struct Fx {
    h: crate::session_store::SessionHandle,
    home: aterm_tempfile::TempDir,
    state: aterm_tempfile::TempDir,
    root: aterm_tempfile::TempDir,
    probes: Probes,
}

impl Fx {
    fn new(local: u64) -> Self {
        Self {
            h: crate::session_store::test_handle_on(local, -1),
            home: aterm_tempfile::tempdir().expect("home"),
            state: aterm_tempfile::tempdir().expect("state"),
            root: aterm_tempfile::tempdir().expect("root"),
            probes: Probes::default(),
        }
    }

    fn ctx(&self) -> &crate::SessionCtx {
        self.h.ctx.as_ref()
    }

    /// The standing proven codex-alab claim of the stand-in on [`CONN`].
    fn claim(&self) {
        claim_supervisor_as(
            self.ctx(),
            &me(),
            Some(CONN),
            None,
            Some(crate::self_claim::stand_in::running()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("a proven claim");
    }

    fn release(&self) {
        assert!(release_supervisor(
            self.ctx(),
            None,
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl
        ));
    }

    /// `rest` asked as `owner` on `conn` with `prove`, the subject `subject` and a wait of the
    /// hang detector (a read that must end answers within it).
    fn ask(
        &self,
        rest: &str,
        owner: bool,
        conn: Option<u64>,
        prove: &dyn Fn(&str) -> Option<Proven>,
        subject: &dyn Fn(&str, &Proven) -> Option<Subject>,
    ) -> String {
        cmd(
            self.ctx(),
            rest,
            owner,
            conn,
            prove,
            &Reads {
                home: Some(self.home.path().to_path_buf()),
                state: Some(self.state.path().to_path_buf()),
                subject,
                probes: &self.probes,
                wait: aterm_spec::wait::MUST_HAPPEN,
            },
        )
    }

    /// What the window harness's own probe reads of `dir` under this home and state root.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn in_process(&self, dir: &Path) -> RepoRead {
        crate::harness_host::session_repo_work_under(
            dir,
            Some(self.home.path()),
            Some(self.state.path()),
            &Used::default(),
        )
    }
}

/// `key=`'s value in a reply line.
fn field<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    reply
        .trim_end()
        .split(' ')
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// A subject at `dir`, as the window harness's snapshot would name it.
fn at(dir: &Path) -> Subject {
    Subject {
        dir: dir.to_path_buf(),
        used: Used::default(),
        source: Source::Snapshot,
    }
}

/// THE PROBE ANSWERS WHAT THE IN-PROCESS PROBE READS: for the claimant's tab, a repository with a
/// changed file, an untracked one and a commit no remote has answers `read=found` with the very
/// counts, repository, remote main and state the window harness's own reader
/// (`session_repo_work_under`, which `Acts::repo_work` reads through) finds there; once that
/// work is committed and pushed, `read=checked clean=1` names the branch, commit and remote main
/// that reader measured. The subject is asked for the claimant's own proof.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn harness_probe_repo_answers_what_the_in_process_probe_reads() {
    let fx = Fx::new(9_611);
    fx.claim();
    let work = pushed_repo(fx.root.path());
    std::fs::write(work.join("b.txt"), "two\n").expect("b.txt");
    git(&work, &["add", "b.txt"]);
    git(&work, &["commit", "-q", "-m", "two"]);
    std::fs::write(work.join("a.txt"), "changed\n").expect("a.txt");
    std::fs::write(work.join("c.txt"), "new\n").expect("c.txt");
    let asked = Cell::new(0);
    let subject = |sid: &str, proven: &Proven| {
        assert_eq!(sid, fx.ctx().self_id.as_str());
        assert_eq!(proven, &crate::self_claim::stand_in::running());
        asked.set(asked.get() + 1);
        Some(at(&work))
    };
    let reply = fx.ask(PROBE, true, Some(CONN), &no_proof, &subject);
    assert_eq!(asked.get(), 1);
    let found = match fx.in_process(&work) {
        RepoRead::Found(found) => found,
        other => panic!("the in-process probe finds the work: {other:?}"),
    };
    assert_eq!(
        (
            found.changed,
            found.untracked,
            found.unpushed,
            found.unlanded
        ),
        (1, 1, 1, 1)
    );
    assert!(
        reply.starts_with("OK read=found clean=0 as_of_ms="),
        "{reply}"
    );
    assert!(
        reply.ends_with('\n') && !reply.trim_end().contains('\n'),
        "{reply}"
    );
    assert_eq!(field(&reply, "from"), Some("snapshot"), "{reply}");
    let count = |n: usize| n.to_string();
    for (key, want) in [
        ("repo", pct_encode(&found.repo)),
        ("main_ref", pct_encode(&found.main_ref)),
        ("changed", count(found.changed)),
        ("untracked", count(found.untracked)),
        ("stashes", count(found.stashes)),
        ("unpushed", count(found.unpushed)),
        ("unlanded", count(found.unlanded)),
        ("sub_unpushed", count(found.sub_unpushed)),
        ("parked", count(found.parked)),
        (
            "state",
            format!("{:016x}", found.state.expect("a known state")),
        ),
        ("path_count", count(found.paths.len())),
        ("path_0", pct_encode(&found.paths[0])),
        ("path_1", pct_encode(&found.paths[1])),
        ("sub_count", "0".to_string()),
        ("parked_on_count", "0".to_string()),
        ("branch", "-".to_string()),
        ("head", "-".to_string()),
        ("remote", "-".to_string()),
    ] {
        assert_eq!(field(&reply, key), Some(want.as_str()), "{key}: {reply}");
    }
    assert_eq!(field(&reply, "main_ref"), Some("origin/main"), "{reply}");

    std::fs::remove_file(work.join("c.txt")).expect("c.txt");
    git(&work, &["commit", "-q", "-am", "three"]);
    git(&work, &["push", "-q", "origin", "HEAD:main"]);
    let reply = fx.ask(PROBE, true, Some(CONN), &no_proof, &subject);
    let checked = match fx.in_process(&work) {
        RepoRead::Checked(checked) => checked,
        other => panic!("the in-process probe checks the pushed work clean: {other:?}"),
    };
    assert!(
        reply.starts_with("OK read=checked clean=1 as_of_ms="),
        "{reply}"
    );
    for (key, want) in [
        ("repo", pct_encode(&checked.repo)),
        ("branch", pct_encode(&checked.branch)),
        ("head", checked.head.clone()),
        ("main_ref", pct_encode(&checked.main_ref)),
        ("remote", "1".to_string()),
        ("changed", "0".to_string()),
        ("untracked", "0".to_string()),
        ("unpushed", "0".to_string()),
        ("unlanded", "0".to_string()),
        ("state", "-".to_string()),
        ("path_count", "0".to_string()),
    ] {
        assert_eq!(field(&reply, key), Some(want.as_str()), "{key}: {reply}");
    }
    assert_eq!(field(&reply, "branch"), Some("main"), "{reply}");
    assert_eq!(checked.head.len(), 40, "{checked:?}");
}

/// ONLY A PROVEN HARNESS CLAIM IN THE TAB READS: an Owner line with no claim, under an unproven
/// claim, on another connection the proof does not name, or from a line that is not the
/// Owner's, answers `ERR denied` and reads nothing (the subject is never asked). CONTROL: the
/// same line on the proven claim's connection, from a process the proof names, and under a
/// rostered harness's contract 3 claim reads; a line that is no `probe repo` is the usage line.
#[test]
fn a_probe_from_an_unclaimed_connection_is_refused() {
    let fx = Fx::new(9_612);
    let dir = fx.root.path().to_path_buf();
    let asked = Cell::new(0);
    let subject = |_: &str, _: &Proven| {
        asked.set(asked.get() + 1);
        Some(at(&dir))
    };
    let denied = "ERR denied\n";
    let read = |reply: &str| reply.starts_with("OK read=");

    assert_eq!(
        fx.ask(PROBE, true, Some(CONN), &no_proof, &subject),
        denied,
        "no claim"
    );
    claim_supervisor_as(
        fx.ctx(),
        &me(),
        Some(CONN),
        None,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("an unproven claim");
    assert_eq!(
        fx.ask(PROBE, true, Some(CONN), &no_proof, &subject),
        denied,
        "an unproven claim"
    );
    fx.release();
    fx.claim();
    assert_eq!(
        fx.ask(PROBE, true, Some(CONN + 1), &no_proof, &subject),
        denied,
        "another connection"
    );
    let other = |_: &str| {
        Some(Proven {
            pid: 4_243,
            start: "another birth".to_string(),
        })
    };
    assert_eq!(
        fx.ask(PROBE, true, Some(CONN + 1), &other, &subject),
        denied,
        "another process's proof"
    );
    assert_eq!(
        fx.ask(PROBE, false, Some(CONN), &no_proof, &subject),
        denied,
        "not the Owner's"
    );
    assert_eq!(asked.get(), 0, "a refused line reads nothing");

    assert!(
        read(&fx.ask(PROBE, true, Some(CONN), &no_proof, &subject)),
        "the claim's connection"
    );
    let named = |_: &str| Some(crate::self_claim::stand_in::running());
    assert!(
        read(&fx.ask(PROBE, true, None, &named, &subject)),
        "the proven process"
    );
    fx.release();
    claim_supervisor_scoped(
        fx.ctx(),
        "grok-alab@4242",
        Some(CONN),
        crate::session_timeline::scoped_input_tests::scoped(&[
            aterm_agent::harness::contract::Scope::Relaunch,
        ]),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("a rostered claim");
    assert!(
        read(&fx.ask(PROBE, true, Some(CONN), &no_proof, &subject)),
        "a contract 3 claim"
    );
    assert_eq!(asked.get(), 3);

    for rest in ["probe", "probe repo now", "probe work"] {
        assert_eq!(
            fx.ask(rest, true, Some(CONN), &no_proof, &subject),
            crate::harness_recovery_verb::USAGE,
            "{rest}"
        );
    }
    assert_eq!(asked.get(), 3, "a usage line reads nothing");
}

/// A TAB WHOSE SUBJECT NAMES NO FOLDER READS CLEAN, as the window harness's probe with no
/// snapshot reads `RepoRead::Clean`, and says it named none (`from=none`).
#[test]
fn a_tab_with_no_folder_reads_clean_and_says_so() {
    let fx = Fx::new(9_613);
    fx.claim();
    let reply = fx.ask(PROBE, true, Some(CONN), &no_proof, &|_, _| None);
    assert!(
        reply.starts_with("OK read=clean clean=1 as_of_ms="),
        "{reply}"
    );
    assert_eq!(field(&reply, "from"), Some("none"), "{reply}");
    assert_eq!(field(&reply, "repo"), Some("-"), "{reply}");
    assert_eq!(field(&reply, "changed"), Some("0"), "{reply}");
}

/// How many tabs' reads `probes` keeps.
fn tabs_kept(probes: &Probes) -> usize {
    probes
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .len()
}

/// `PROBE` asked on the claim's connection with the subject `dir` (as the window harness's
/// snapshot names it) and a wait of `wait`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn probe_within(fx: &Fx, dir: &Path, wait: std::time::Duration) -> String {
    cmd(
        fx.ctx(),
        PROBE,
        true,
        Some(CONN),
        &no_proof,
        &Reads {
            home: Some(fx.home.path().to_path_buf()),
            state: Some(fx.state.path().to_path_buf()),
            subject: &|_, _| Some(at(dir)),
            probes: &fx.probes,
            wait,
        },
    )
}

/// Begins, on tab `sid`'s reads of the subject `dir` in `probes`, a read that goes on until the
/// returned sender sends (or drops), then answers the folder `held` gone, begun at 7 ms.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn hold_read(probes: &Probes, sid: &str, dir: &Path) -> std::sync::mpsc::Sender<()> {
    let (go, gate) = std::sync::mpsc::channel::<()>();
    let probing = probes.of(sid, &at(dir));
    let began = probing.ask(
        move || {
            move || {
                let _ = gate.recv();
                Timed {
                    read: RepoRead::Gone("held".to_string()),
                    began_ms: Some(7),
                }
            }
        },
        std::time::Duration::ZERO,
        &NEVER_STOPPED,
    );
    assert!(began.is_none(), "the held read goes on: {began:?}");
    go
}

/// A READ THAT OUTLASTS ITS ASK IS `read=pending`, AND THE TAB'S NEXT ASK OF THAT SUBJECT TAKES
/// ITS ANSWER: an ask whose wait ends while the tab's read goes on answers exactly `OK
/// read=pending from=snapshot`, begins no other read, and the next ask once it ended answers
/// that very read (the held one's `gone`, begun at 7 ms), the one after a fresh read of the
/// repository. A TAB ASKED ABOUT ANOTHER SUBJECT DROPS ITS OLD READ: with a held read of `a`
/// ended unasked, an ask of `b` answers a read of `b`, not the held answer, and the next ask of
/// `a` a fresh read of `a`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_read_that_outlasts_its_ask_is_pending_and_the_next_ask_answers_it() {
    let fx = Fx::new(9_614);
    fx.claim();
    let sid = fx.ctx().self_id.as_str().to_string();
    let a = pushed_repo(&fx.root.path().join("a"));
    let b = pushed_repo(&fx.root.path().join("b"));
    let checked_repo = |dir: &Path| match fx.in_process(dir) {
        RepoRead::Checked(at) => pct_encode(&at.repo),
        other => panic!("the in-process probe checks {dir:?} clean: {other:?}"),
    };
    let (repo_a, repo_b) = (checked_repo(&a), checked_repo(&b));
    assert_ne!(repo_a, repo_b);
    let must = aterm_spec::wait::MUST_HAPPEN;

    let go = hold_read(&fx.probes, &sid, &a);
    assert_eq!(
        probe_within(&fx, &a, std::time::Duration::ZERO),
        "OK read=pending from=snapshot\n"
    );
    go.send(()).expect("the held read waits");
    let reply = probe_within(&fx, &a, must);
    assert!(
        reply.starts_with("OK read=gone clean=1 as_of_ms=7 from=snapshot repo=held "),
        "the held read's answer: {reply}"
    );
    let reply = probe_within(&fx, &a, must);
    assert!(reply.starts_with("OK read=checked clean=1 "), "{reply}");
    assert_eq!(field(&reply, "repo"), Some(repo_a.as_str()), "{reply}");

    let go = hold_read(&fx.probes, &sid, &a);
    go.send(()).expect("the held read waits");
    let reply = probe_within(&fx, &b, must);
    assert!(reply.starts_with("OK read=checked clean=1 "), "{reply}");
    assert_eq!(field(&reply, "repo"), Some(repo_b.as_str()), "{reply}");
    let reply = probe_within(&fx, &a, must);
    assert!(reply.starts_with("OK read=checked clean=1 "), "{reply}");
    assert_eq!(field(&reply, "repo"), Some(repo_a.as_str()), "{reply}");
    assert_eq!(tabs_kept(&fx.probes), 1, "one tab, one read");
}

/// AT MOST `PROBED_TABS` TABS' READS ARE KEPT, THE TAB ASKED ABOUT LONGEST AGO DROPPED FIRST:
/// each of `PROBED_TABS` tabs asked again finds its own reads; once the first is asked again and
/// one more tab is asked, the second (now the least recently asked) begins anew while the first
/// keeps its reads. A tab asked about another subject keeps one entry, of the new subject.
#[test]
fn the_reads_keep_the_tabs_asked_about_last() {
    let probes = Probes::default();
    let here = at(Path::new("/here"));
    let tab = |i: usize| format!("s-{i}");
    let kept: Vec<_> = (0..PROBED_TABS)
        .map(|i| probes.of(&tab(i), &here))
        .collect();
    for (i, reads) in kept.iter().enumerate() {
        assert!(
            Arc::ptr_eq(reads, &probes.of(&tab(i), &here)),
            "tab {i} kept"
        );
    }
    assert!(Arc::ptr_eq(&kept[0], &probes.of(&tab(0), &here)));
    probes.of(&tab(PROBED_TABS), &here);
    assert_eq!(tabs_kept(&probes), PROBED_TABS);
    assert!(
        Arc::ptr_eq(&kept[0], &probes.of(&tab(0), &here)),
        "the tab asked again keeps its reads"
    );
    assert!(
        !Arc::ptr_eq(&kept[1], &probes.of(&tab(1), &here)),
        "the tab asked about longest ago was dropped"
    );
    assert_eq!(tabs_kept(&probes), PROBED_TABS);

    let probes = Probes::default();
    let there = at(Path::new("/there"));
    let first = probes.of("s-moved", &here);
    let moved = probes.of("s-moved", &there);
    assert!(!Arc::ptr_eq(&first, &moved), "a moved subject begins anew");
    assert!(Arc::ptr_eq(&moved, &probes.of("s-moved", &there)));
    assert_eq!(tabs_kept(&probes), 1);
}

/// A published snapshot of `sid`'s agent `pid` born `start`, working in `cwd`, with no
/// conversation (a codex-alab's run, whose transcript nothing reads).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn snapshot_of(sid: &str, pid: u32, start: &str, cwd: &Path, codex: &Path) -> Snapshot {
    Snapshot {
        lifetime: None,
        tab: sid.to_string(),
        pid,
        start: start.to_string(),
        shell: 1,
        program: PathBuf::from("/opt/codex-alab"),
        argv: vec!["/opt/codex-alab".to_string()],
        session: None,
        cwd: cwd.display().to_string(),
        version: None,
        run: aterm_agent::harness::relaunch::AgentRun::Codex(
            aterm_agent::harness::relaunch::CodexRun {
                home: codex.to_path_buf(),
                home_env: None,
                self_claimed: None,
            },
        ),
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        auth_observed: false,
        profile: None,
        launch_env: None,
    }
}

/// Takes `sid`'s published snapshot down when dropped.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Unpublish<'a>(&'a str);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for Unpublish<'_> {
    fn drop(&mut self) {
        crate::harness_host::publish_snapshot_for_test(self.0, None);
    }
}

/// THE LIVE SUBJECT IS THE SNAPSHOT THAT NAMES THE CLAIMANT, ELSE ITS LIVING PROCESS'S FOLDER:
/// the window harness's published snapshot of the tab whose pid and start are the proof's is read
/// as the window harness's own probe reads it (`from=snapshot`, its folder, what the session used
/// since the snapshot's start); with no snapshot, or one of an earlier agent in the tab (another
/// pid, or this pid at another birth), it is the claimant's own working directory
/// (`from=claimant`), what it used since its start; a proof whose process is gone (its pid at
/// another birth) names nothing, though that pid has a folder.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_live_subject_is_the_snapshot_naming_the_claimant_else_its_living_folder() {
    let fx = Fx::new(9_615);
    let sid = fx.ctx().self_id.as_str().to_string();
    let _unpublish = Unpublish(&sid);
    let proven = crate::self_claim::stand_in::running();
    let used = Used::of_session(
        aterm_agent::harness::unix_time::parse_lstart(&proven.start),
        None,
    );
    let own = aterm_sysprobe::process_cwd(proven.pid).expect("the stand-in's folder");
    let claimant = Some(Subject {
        dir: own,
        used: used.clone(),
        source: Source::Claimant,
    });
    assert_eq!(live_subject(&sid, &proven), claimant, "no snapshot");

    let snapped = fx.root.path().join("snapped");
    let codex = fx.home.path().join(".codex");
    let publish = |pid: u32, start: &str| {
        crate::harness_host::publish_snapshot_for_test(
            &sid,
            Some(&snapshot_of(&sid, pid, start, &snapped, &codex)),
        );
    };
    publish(proven.pid, &proven.start);
    assert_eq!(
        live_subject(&sid, &proven),
        Some(Subject {
            dir: snapped.clone(),
            used,
            source: Source::Snapshot,
        }),
        "the snapshot that names the claimant"
    );
    publish(proven.pid + 1, &proven.start);
    assert_eq!(live_subject(&sid, &proven), claimant, "another pid's");
    publish(proven.pid, "Thu Jan  1 00:00:01 1970");
    assert_eq!(live_subject(&sid, &proven), claimant, "another birth's");

    crate::harness_host::publish_snapshot_for_test(&sid, None);
    let reborn = Proven {
        pid: proven.pid,
        start: "Thu Jan  1 00:00:01 1970".to_string(),
    };
    assert!(aterm_sysprobe::process_cwd(reborn.pid).is_some());
    assert_eq!(
        live_subject(&sid, &reborn),
        None,
        "a proof whose process is gone"
    );
}
