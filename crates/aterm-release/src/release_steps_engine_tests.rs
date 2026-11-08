// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release's own publication engine ([`crate::gates::engine_copy`]) against a stand-in
//! engine: an origin, the checkout other processes on the Mac align (`~/publication`), a peer
//! that lands engine code on the origin, and a release clone. The stand-in's `bin/pub` keeps the
//! two rules of the real engine's stage that decide this (`bin/pub`'s capture and
//! `revalidate_push_inputs`): HEAD is the origin's main when the stage starts, and a HEAD that
//! moved by anything but the data plane while it ran refuses the push.

use super::*;

const SHARED_NAME: &str = "Engine Owner";
const SHARED_EMAIL: &str = "owner@example.invalid";

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git -C {} {args:?}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_exec(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// The engine's origin, the shared checkout, a peer, a release clone and its candidate.
struct World {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
    origin: PathBuf,
    shared: PathBuf,
    peer: PathBuf,
    repo: PathBuf,
    candidate: Candidate,
    prover: Prover,
}

impl World {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (origin, shared, peer, repo) = (
            root.join("publication.git"),
            root.join("publication"),
            root.join("peer"),
            root.join("aterm"),
        );
        let report = root.join("scanner-report.json");
        std::fs::write(&report, scanner_tools::test_output().stdout).unwrap();
        git_in(
            &root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &origin.display().to_string(),
            ],
        );
        crate::ledger::testing::init(
            &shared,
            &[("user.name", SHARED_NAME), ("user.email", SHARED_EMAIL)],
        );
        git_in(
            &shared,
            &["remote", "add", "origin", &origin.display().to_string()],
        );
        // What `pub stage` decides this by, from the checkout it runs in (`here`), in the
        // engine's words; a stage appends its audit row to the data plane, as the engine does.
        write_exec(
            &shared.join("bin/pub"),
            &format!(
                "#!/bin/sh\nhere=$(cd \"$(dirname \"$0\")/..\" && pwd)\n\
                 case \"$1\" in\n\
                 scanner-preflight) cat '{report}'; exit 0 ;;\n\
                 stage)\n\
                 captured=$(git -C \"$here\" rev-parse HEAD) || exit 12\n\
                 remote=$(git --git-dir='{origin}' rev-parse refs/heads/main) || exit 12\n\
                 [ \"$captured\" = \"$remote\" ] || {{ echo \"FAIL: publication engine HEAD \
                 $captured does not exactly match queried origin/main $remote\"; exit 1; }}\n\
                 [ -x '{mid}' ] && '{mid}' \"$here\"\n\
                 now=$(git -C \"$here\" rev-parse HEAD) || exit 12\n\
                 if [ \"$now\" != \"$captured\" ]; then\n\
                 code=$(git -C \"$here\" diff --name-only \"$captured\" \"$now\" | grep -v -x -e \
                 mappings.json -e verification-log.jsonl | grep -v '^handoff/')\n\
                 [ -n \"$code\" ] && {{ echo \"FAIL: publication engine HEAD changed during stage \
                 (non-data-plane paths: $code); refusing to push\"; exit 1; }}\n\
                 fi\n\
                 printf '%s\\n' '{{\"stage\":\"aterm\"}}' >> \"$here/verification-log.jsonl\"\n\
                 echo \"== staged by the engine at $here\"; exit 0 ;;\n\
                 esac\nexit 9\n",
                report = report.display(),
                origin = origin.display(),
                mid = root.join("mid-stage").display(),
            ),
        );
        std::fs::write(shared.join("mappings.json"), "{\"mappings\":{}}\n").unwrap();
        std::fs::write(shared.join("verification-log.jsonl"), "").unwrap();
        git_in(&shared, &["add", "-A"]);
        git_in(&shared, &["commit", "-q", "-m", "engine"]);
        git_in(&shared, &["push", "-q", "-u", "origin", "main"]);
        git_in(
            &root,
            &["clone", "-q", &origin.display().to_string(), "peer"],
        );
        crate::ledger::testing::config(
            &peer,
            &[
                ("user.name", "Peer"),
                ("user.email", "peer@example.invalid"),
            ],
        );
        crate::ledger::testing::init(
            &repo,
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
            ],
        );
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "candidate"]);
        let candidate = Candidate {
            commit: git_in(&repo, &["rev-parse", "HEAD"]),
            version: "1.0.0".into(),
            why: "test".into(),
            bump_from: None,
            carry: Vec::new(),
        };
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink("/usr/bin/false", bin.join("targo")).unwrap();
        let prover = Prover {
            bin,
            commit: "test".into(),
            source: "test".into(),
        };
        Self {
            _dir: dir,
            root,
            origin,
            shared,
            peer,
            repo,
            candidate,
            prover,
        }
    }

    /// The release's own engine, where [`crate::gates::engine_copy`] puts it.
    fn copy(&self) -> PathBuf {
        crate::gates::engine_copy(&crate::gates::release_dir(&self.repo))
    }

    /// A real run's backend named the shared checkout (`dry`: a dry run's), its logs its own.
    fn real(&self, dry: bool) -> Real {
        let mut real = Real::new(&self.repo, Some(&self.shared), dry, false).unwrap();
        real.prerequisite_acquire = crate::managed_prerequisites::simulated;
        real.scanner_probe = |_| Ok(scanner_tools::test_output());
        real.logs = self.root.join(format!("logs-{}", u8::from(dry)));
        std::fs::create_dir_all(&real.logs).unwrap();
        real
    }

    /// What the stage runs while it runs (`$1`: the checkout it runs in).
    fn mid_stage(&self, script: &str) {
        write_exec(
            &self.root.join("mid-stage"),
            &format!("#!/bin/sh\nset -e\n{script}"),
        );
    }

    /// The peer lands engine code (`bin/<name>`) on the origin.
    fn peer_lands_code(&self, name: &str) {
        git_in(&self.peer, &["pull", "-q", "--ff-only"]);
        std::fs::write(self.peer.join("bin").join(name), "# peer engine code\n").unwrap();
        git_in(&self.peer, &["add", "-A"]);
        git_in(&self.peer, &["commit", "-q", "-m", "peer engine code"]);
        git_in(&self.peer, &["push", "-q", "origin", "HEAD:main"]);
    }

    /// A peer's line, then a second writer fast-forwarding `checkout` to the origin's main: what
    /// moved `~/publication` at 18:12 under 0.172.0's stage.
    fn lands_and_aligns(&self, name: &str, checkout: &str) -> String {
        format!(
            "git -C '{peer}' pull -q --ff-only\n\
             printf '# peer engine code\\n' > '{peer}/bin/{name}'\n\
             git -C '{peer}' add -A\n\
             git -C '{peer}' -c commit.gpgsign=false commit -q -m 'peer engine code'\n\
             git -C '{peer}' push -q origin HEAD:main\n\
             git -C {checkout} fetch -q origin\n\
             git -C {checkout} merge -q --ff-only origin/main\n",
            peer = self.peer.display(),
        )
    }

    fn origin_main(&self) -> String {
        git_in(&self.origin, &["rev-parse", "refs/heads/main"])
    }

    fn log(&self, real: &Real) -> String {
        std::fs::read_to_string(real.step_log(Step::Publish)).unwrap_or_default()
    }
}

/// A SECOND WRITER FAST-FORWARDING THE SHARED ENGINE CHECKOUT MID-STAGE LEAVES THE RELEASE'S
/// STAGE UNAFFECTED: the release stages from its own engine, which nobody else moves, so the
/// engine's own guard passes while a peer's engine code lands and `~/publication` is aligned to
/// it under the stage; the stage's audit row still reaches the engine's origin, and nothing of
/// the release's reaches the shared checkout. NEGATIVE CONTROL: the same stage run in the shared
/// checkout, as the release ran it before (its engine the checkout it was named), is refused by
/// that guard: "publication engine HEAD changed during stage (non-data-plane paths: …)".
#[test]
fn a_second_writer_moving_the_shared_engine_mid_stage_leaves_the_stage_unaffected() {
    let w = World::new();
    w.mid_stage(&w.lands_and_aligns("pub-admit", &format!("'{}'", w.shared.display())));
    let shared_reflog = git_in(&w.shared, &["reflog", "--format=%gs"]);
    let mut real = w.real(false);
    assert_eq!(real.engine, w.copy(), "a real run's engine is its own");
    let ran = Backend::stage(&mut real, &w.candidate, &w.prover, StageTarget::Remote).unwrap();
    let log = w.log(&real);
    assert_eq!(ran.code, 0, "{ran:?}\n{log}");
    assert!(
        log.contains(&format!(
            "== staged by the engine at {}",
            w.copy().display()
        )),
        "{log}"
    );
    // The second writer did move the shared checkout under the stage, by the peer's code alone.
    assert_eq!(
        git_in(&w.shared, &["rev-parse", "HEAD"]),
        git_in(&w.peer, &["rev-parse", "HEAD"])
    );
    assert!(w.shared.join("bin/pub-admit").is_file());
    let moved = git_in(&w.shared, &["reflog", "--format=%gs"]);
    assert_eq!(
        moved.lines().skip(1).collect::<Vec<_>>(),
        shared_reflog.lines().collect::<Vec<_>>(),
        "only the second writer moved it"
    );
    assert_eq!(git_in(&w.shared, &["status", "--porcelain"]), "");
    assert_eq!(
        std::fs::read_to_string(w.shared.join("verification-log.jsonl")).unwrap(),
        "",
        "the release's audit row is not the shared checkout's"
    );
    // The stage's audit row reached the engine's origin from the release's engine, which the
    // sync after the stage brought to the origin's main, the peer's code with it.
    assert_eq!(
        git_in(&w.origin, &["show", "main:verification-log.jsonl"]),
        "{\"stage\":\"aterm\"}"
    );
    assert_eq!(
        git_in(&w.origin, &["log", "-1", "--format=%s %an", "main"]),
        format!("chore(audit): record stage verification rows {SHARED_NAME}")
    );
    assert_eq!(git_in(&w.copy(), &["rev-parse", "HEAD"]), w.origin_main());
    assert_eq!(git_in(&w.copy(), &["status", "--porcelain"]), "");

    // NEGATIVE CONTROL: the release's engine as the checkout it was named.
    w.mid_stage(&w.lands_and_aligns("pub-ledger", &format!("'{}'", w.shared.display())));
    real.engine = w.shared.clone();
    let ran = Backend::stage(&mut real, &w.candidate, &w.prover, StageTarget::Remote).unwrap();
    let log = w.log(&real);
    assert_eq!(ran.code, 1, "{ran:?}\n{log}");
    assert!(
        log.contains(
            "publication engine HEAD changed during stage (non-data-plane paths: bin/pub-ledger)"
        ),
        "{log}"
    );
}

/// THE RELEASE'S OWN ENGINE STILL REFUSES CODE MOVING ITS OWN HEAD MID-STAGE: the engine runs in
/// the copy, so its guard watches the copy, and a fast-forward of the copy to a peer's engine
/// code under the stage is refused, while one of the data plane alone (a ledger row) passes.
/// NEGATIVE CONTROL: the same peer line, the shared checkout aligned instead, passes (the test
/// above), so the refusal here is the copy's HEAD, not the line.
#[test]
fn the_releases_own_engine_still_refuses_code_moving_its_head_mid_stage() {
    let w = World::new();
    let mut real = w.real(false);
    w.mid_stage(&w.lands_and_aligns("pub-admit", "\"$1\""));
    let ran = Backend::stage(&mut real, &w.candidate, &w.prover, StageTarget::Remote).unwrap();
    let log = w.log(&real);
    assert_eq!(ran.code, 1, "{ran:?}\n{log}");
    assert!(
        log.contains(
            "publication engine HEAD changed during stage (non-data-plane paths: bin/pub-admit)"
        ),
        "{log}"
    );
    // A ledger row landing under the stage moves the copy by the data plane alone.
    w.mid_stage(&format!(
        "git -C '{peer}' pull -q --ff-only\n\
         printf '{{\"mappings\":{{\"other\":{{}}}}}}\\n' > '{peer}/mappings.json'\n\
         git -C '{peer}' -c commit.gpgsign=false commit -q -am 'feat(ledger): other'\n\
         git -C '{peer}' push -q origin HEAD:main\n\
         git -C \"$1\" fetch -q origin\n\
         git -C \"$1\" merge -q --ff-only origin/main\n",
        peer = w.peer.display()
    ));
    let ran = Backend::stage(&mut real, &w.candidate, &w.prover, StageTarget::Remote).unwrap();
    let log = w.log(&real);
    assert_eq!(ran.code, 0, "{ran:?}\n{log}");
}

/// STEP 2 MAKES THE RELEASE'S ENGINE FROM ITS ORIGIN AND NEVER WRITES THE CHECKOUT IT WAS NAMED:
/// the copy is a clone of the origin the shared checkout names, on main at the origin's main
/// (not at the shared checkout's own HEAD, which is behind it), committing as the shared
/// checkout's committer; a later sync fast-forwards it and follows the origin the shared
/// checkout is re-pointed at. The shared checkout's branch and changes are its users': step 1
/// asks nothing of them, and no sync touches them. NEGATIVE CONTROL: the same changes in the
/// copy refuse step 1 and the sync.
#[test]
fn step_two_makes_the_releases_engine_from_its_origin_and_never_writes_the_named_checkout() {
    let w = World::new();
    w.peer_lands_code("pub-later");
    git_in(&w.shared, &["switch", "-q", "-c", "experiment"]);
    std::fs::write(w.shared.join("bin/pub"), "#!/bin/sh\nexit 99\n").unwrap();
    let before = (
        git_in(&w.shared, &["rev-parse", "HEAD"]),
        git_in(&w.shared, &["status", "--porcelain"]),
        git_in(&w.shared, &["for-each-ref"]),
        git_in(&w.shared, &["reflog", "--format=%gs"]),
    );
    let real = w.real(false);
    assert!(
        real.engine_unready().is_none(),
        "the shared checkout's state is not the release's"
    );
    assert!(!w.copy().exists(), "step 1 makes nothing");
    real.engine_sync().unwrap();
    let copy = w.copy();
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), w.origin_main());
    assert_ne!(
        before.0,
        w.origin_main(),
        "the shared checkout is behind its origin"
    );
    assert_eq!(git_in(&copy, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(
        git_in(&copy, &["rev-parse", "--abbrev-ref", "main@{upstream}"]),
        "origin/main"
    );
    assert_eq!(
        git_in(&copy, &["remote", "get-url", "origin"]),
        w.origin.display().to_string()
    );
    assert_eq!(git_in(&copy, &["config", "user.name"]), SHARED_NAME);
    assert_eq!(git_in(&copy, &["config", "user.email"]), SHARED_EMAIL);
    assert!(copy.join("bin/pub-later").is_file());
    assert_eq!(git_in(&copy, &["status", "--porcelain"]), "");
    let leftovers: Vec<_> = std::fs::read_dir(copy.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("engine.new-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // A later sync fast-forwards it, and follows the origin the shared checkout is pointed at.
    w.peer_lands_code("pub-next");
    let mirror = w.root.join("mirror.git");
    git_in(
        &w.root,
        &[
            "clone",
            "-q",
            "--bare",
            &w.origin.display().to_string(),
            "mirror.git",
        ],
    );
    git_in(
        &w.shared,
        &["remote", "set-url", "origin", &mirror.display().to_string()],
    );
    real.engine_sync().unwrap();
    assert_eq!(
        git_in(&copy, &["remote", "get-url", "origin"]),
        mirror.display().to_string()
    );
    assert_eq!(
        git_in(&copy, &["rev-parse", "HEAD"]),
        git_in(&mirror, &["rev-parse", "refs/heads/main"])
    );
    git_in(
        &w.shared,
        &[
            "remote",
            "set-url",
            "origin",
            &w.origin.display().to_string(),
        ],
    );
    // Holding nothing the mirror lacks, the copy is made again from the origin named now.
    real.engine_sync().unwrap();
    assert_eq!(
        git_in(&copy, &["remote", "get-url", "origin"]),
        w.origin.display().to_string()
    );
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), w.origin_main());
    let after = (
        git_in(&w.shared, &["rev-parse", "HEAD"]),
        git_in(&w.shared, &["status", "--porcelain"]),
        git_in(&w.shared, &["for-each-ref"]),
        git_in(&w.shared, &["reflog", "--format=%gs"]),
    );
    assert_eq!(
        before, after,
        "nothing the release did reached the shared checkout"
    );

    // NEGATIVE CONTROL: the same change in the release's engine refuses step 1 and the sync.
    std::fs::write(copy.join("bin/pub"), "#!/bin/sh\nexit 99\n").unwrap();
    let unready = real.engine_unready().expect("a changed copy is refused");
    assert!(unready.why.contains("has local changes"), "{unready:?}");
    let refused = real.engine_sync().unwrap_err();
    assert!(refused.why.contains("has local changes"), "{refused:?}");
    git_in(&copy, &["checkout", "-q", "--", "bin/pub"]);
    assert!(real.engine_unready().is_none());
}

/// THE RELEASE'S ENGINE DELIVERS THE RECORDS IT HOLDS UNPUSHED, AND KEEPS ANYTHING ELSE: nothing
/// but a release writes its engine, so a ledger row `pub publish` committed and could not push,
/// or an audit row whose push failed, is merged with the origin's main and pushed by the next
/// sync, not stranded (no other process aligns this checkout and pushes it). NEGATIVE CONTROLS:
/// a commit that changes engine code, and rows that conflict with the origin's, are refused, the
/// copy's HEAD left as it was, and nothing pushed.
#[test]
fn the_releases_engine_delivers_the_records_it_holds_unpushed_and_keeps_anything_else() {
    let w = World::new();
    let real = w.real(false);
    real.engine_sync().unwrap();
    let copy = w.copy();
    let commit = |path: &str, text: &str, subject: &str| {
        std::fs::write(copy.join(path), text).unwrap();
        git_in(&copy, &["add", path]);
        git_in(&copy, &["commit", "-q", "-m", subject]);
        git_in(&copy, &["rev-parse", "HEAD"])
    };
    // A ledger row whose push failed, the origin moved on by a peer's line meanwhile.
    commit(
        "mappings.json",
        "{\"mappings\":{\"kept\":{}}}\n",
        "feat(ledger): a row whose push failed",
    );
    w.peer_lands_code("pub-meanwhile");
    real.engine_sync().unwrap();
    assert_eq!(
        git_in(&w.origin, &["show", "main:mappings.json"]),
        "{\"mappings\":{\"kept\":{}}}"
    );
    assert!(
        git_in(&w.origin, &["ls-tree", "--name-only", "main", "bin/"]).contains("pub-meanwhile")
    );
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), w.origin_main());
    // An audit row committed and not pushed, the origin where it was.
    commit(
        "verification-log.jsonl",
        "{\"stage\":\"unpushed\"}\n",
        "chore(audit): record stage verification rows",
    );
    real.engine_sync().unwrap();
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), w.origin_main());
    assert_eq!(
        git_in(&w.origin, &["show", "main:verification-log.jsonl"]),
        "{\"stage\":\"unpushed\"}"
    );

    // NEGATIVE CONTROL: engine code the origin lacks is kept, refused, and not pushed.
    let origin_was = w.origin_main();
    let code = commit("bin/pub-mine", "# mine\n", "engine code nobody landed");
    let log = copy.join("verification-log.jsonl");
    let staged = format!(
        "{}{{\"stage\":\"refused-sync\"}}\n",
        std::fs::read_to_string(&log).unwrap()
    );
    std::fs::write(&log, &staged).unwrap();
    let refused = real.engine_sync().unwrap_err();
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        staged,
        "a stage's audit row stays for the next sync"
    );
    git_in(&copy, &["checkout", "-q", "--", "verification-log.jsonl"]);
    assert!(
        refused
            .why
            .contains("change more than its records (bin/pub-mine)"),
        "{refused:?}"
    );
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), code);
    assert_eq!(w.origin_main(), origin_was);
    git_in(&copy, &["reset", "-q", "--hard", "origin/main"]);
    // NEGATIVE CONTROL: rows that conflict with the origin's are kept, the merge undone.
    let row = commit(
        "mappings.json",
        "{\"mappings\":{\"mine\":{}}}\n",
        "feat(ledger): mine",
    );
    git_in(&w.peer, &["pull", "-q", "--ff-only"]);
    std::fs::write(
        w.peer.join("mappings.json"),
        "{\"mappings\":{\"theirs\":{}}}\n",
    )
    .unwrap();
    git_in(&w.peer, &["commit", "-q", "-am", "feat(ledger): theirs"]);
    git_in(&w.peer, &["push", "-q", "origin", "HEAD:main"]);
    let refused = real.engine_sync().unwrap_err();
    assert!(
        refused.why.contains("do not merge with its origin's main"),
        "{refused:?}"
    );
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), row);
    assert_eq!(git_in(&copy, &["status", "--porcelain"]), "");
    assert_eq!(
        git_in(&w.origin, &["show", "main:mappings.json"]),
        "{\"mappings\":{\"theirs\":{}}}"
    );
}

/// STEP 1 AND A DRY RUN JUDGE THE ORIGIN'S LEDGER; A RUN THAT SYNCED JUDGES ITS OWN ENGINE'S: a
/// row on the origin that neither the shared checkout nor a release behind it holds is what step
/// 1 reads, and once the run has synced, the copy's ledger (which its own `pub publish` writes,
/// here before any push) is what it reads. A dry run makes and moves no engine and judges the
/// origin's code as well (the pre-promote seam step 4 asks of `bin/pub`), never the named
/// checkout's working tree. NEGATIVE CONTROLS: the shared checkout's ledger lacks the row and its
/// `bin/pub` the seam, so a release reading it (as before) would miss the one and refuse on the
/// other.
#[test]
fn step_one_and_a_dry_run_judge_the_origins_ledger_and_a_synced_run_its_own() {
    let w = World::new();
    let r = "a".repeat(40);
    let row = format!(
        "{{\"mappings\":{{\"aterm\":{{\"{r}\":{{\"verified_at\":\"2026-11-07T18:00:00Z\",\
         \"release_rev\":\"e1\"}}}}}}}}\n"
    );
    git_in(&w.peer, &["pull", "-q", "--ff-only"]);
    std::fs::write(w.peer.join("mappings.json"), &row).unwrap();
    git_in(&w.peer, &["commit", "-q", "-am", "feat(ledger): aterm"]);
    git_in(&w.peer, &["push", "-q", "origin", "HEAD:main"]);
    let shared_ledger = std::fs::read_to_string(w.shared.join("mappings.json")).unwrap();
    assert!(
        !engine_rows_in(&shared_ledger, Some(&r)).has,
        "the shared checkout is behind"
    );
    std::fs::write(w.shared.join("bin/pub"), "#!/bin/sh\nexit 99\n").unwrap();

    // A dry run: no engine made, the origin's ledger and code judged.
    let mut dry = w.real(true);
    assert_eq!(dry.engine, w.shared, "what a dry run's children are handed");
    assert_eq!(dry.engine_text("bin/pub"), None, "nothing read yet");
    dry.fetch_engine_origin().unwrap();
    assert!(dry.engine_rows(Some(&r)).has);
    assert!(
        dry.engine_text("bin/pub")
            .is_some_and(|t| t.contains("scanner-preflight"))
    );
    let refused = dry.engine_sync().unwrap_err();
    assert!(
        refused
            .why
            .contains("a dry run makes and moves no publication engine"),
        "{refused:?}"
    );
    assert!(!w.copy().exists());

    // A real run's step 1, before its sync: the origin's ledger, not a release-old copy's, read
    // by step 1's fetch of both origins. NEGATIVE CONTROL: where aterm's origin cannot be read,
    // step 1 refuses before it reads the engine's.
    let mut real = w.real(false);
    assert!(!real.engine_rows(Some(&r)).has, "nothing read yet");
    let unread = real.fetch_origins(|_| Err("no route".into())).unwrap_err();
    assert!(unread.why.contains("cannot refresh origin"), "{unread:?}");
    assert!(
        !real.engine_rows(Some(&r)).has,
        "nothing read after a refusal"
    );
    let fetched = real.fetch_origins(|_| {
        Ok(crate::github_remote::FetchedOrigin {
            main: "m".repeat(40),
            tags: "t\trefs/tags/v1".into(),
        })
    });
    assert_eq!(fetched.unwrap(), "m".repeat(40));
    assert_eq!(real.origin_tags, "t\trefs/tags/v1");
    assert!(
        real.engine_rows(Some(&r)).has,
        "step 1 read the engine's origin"
    );
    real.engine_sync().unwrap();
    // Synced: the release's own engine, with a row its publish wrote and has not pushed.
    let s = "b".repeat(40);
    std::fs::write(w.copy().join("mappings.json"), row.replace(&r, &s)).unwrap();
    assert!(real.engine_rows(Some(&s)).has);
    assert!(!real.engine_rows(Some(&r)).has);
    // Once one exists, a dry run reads the release's engine's code.
    assert_eq!(w.real(true).engine, w.copy());
}

/// EVERY CHILD OF A REAL RUN IS HANDED THE RELEASE'S ENGINE, AND THE RE-RUN AS R'S OWN CUTTER,
/// handed it as its engine, runs it in place: `PUBLICATION_ENGINE` names the copy for each child
/// (`pub` itself, `ship cut`, which reads its ledger, `verify`, which reads its baseline), even
/// before step 2 makes it; a backend named the copy keeps it and syncs it from its own origin.
/// NEGATIVE CONTROL: a dry run with no copy hands the checkout it was named.
#[test]
fn every_child_of_a_real_run_is_handed_the_releases_engine() {
    let w = World::new();
    let named = |cmd: &Command| {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("PUBLICATION_ENGINE"))
            .and_then(|(_, v)| v.map(PathBuf::from))
    };
    let mut real = w.real(false);
    real.prerequisites = Some(crate::managed_prerequisites::Tools::Simulated);
    let copy = w.copy();
    assert_eq!(
        named(&real.child("/bin/true", Some(&w.prover))),
        Some(copy.clone())
    );
    let cut = real.cut_command(&["cut".into()], &w.prover).unwrap();
    assert_eq!(named(&cut), Some(copy.clone()));
    let publish = real.publish_command(&w.prover).unwrap();
    assert_eq!(publish.get_program(), copy.join("bin/pub").as_os_str());
    assert_eq!(named(&publish), Some(copy.clone()));
    assert_eq!(
        named(&w.real(true).child("/bin/true", None)),
        Some(w.shared.clone()),
        "a dry run with no copy hands the checkout it was named"
    );

    real.engine_sync().unwrap();
    // The re-run as R's own cutter: named the copy, it runs it in place, from its own origin.
    w.peer_lands_code("pub-again");
    let mut again = Real::new(&w.repo, Some(&copy), false, false).unwrap();
    again.logs = w.root.join("logs-again");
    std::fs::create_dir_all(&again.logs).unwrap();
    assert_eq!((&again.engine, &again.engine_source), (&copy, &copy));
    assert!(again.engine_unready().is_none());
    again.engine_sync().unwrap();
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), w.origin_main());
    assert_eq!(
        git_in(&copy, &["remote", "get-url", "origin"]),
        w.origin.display().to_string()
    );
}

/// A COPY OF ANOTHER ORIGIN IS MADE AGAIN, AND WHAT IT HELD IS NEVER DELIVERED TO THIS ONE: a
/// release named an engine whose origin is a fork (sharing the engine's history, with ledger
/// rows of its own), then one whose origin is the engine's; the copy, holding nothing the fork
/// lacks, is cloned again from the engine's origin, and the fork's row (records ahead of the
/// engine's main, the shape a failed push leaves) never reaches the engine's main. NEGATIVE
/// CONTROL: a copy holding a row its fork lacks is refused and kept, and pushes nothing anywhere.
#[test]
fn a_copy_of_another_origin_is_made_again_and_never_delivered_to_this_one() {
    let w = World::new();
    let fork = w.root.join("fork.git");
    git_in(
        &w.root,
        &[
            "clone",
            "-q",
            "--bare",
            &w.origin.display().to_string(),
            "fork.git",
        ],
    );
    let point = |at: &Path| {
        git_in(
            &w.shared,
            &["remote", "set-url", "origin", &at.display().to_string()],
        );
    };
    let real = w.real(false);
    point(&fork);
    real.engine_sync().unwrap();
    let forker = w.root.join("forker");
    git_in(
        &w.root,
        &["clone", "-q", &fork.display().to_string(), "forker"],
    );
    std::fs::write(
        forker.join("mappings.json"),
        "{\"mappings\":{\"fork\":{}}}\n",
    )
    .unwrap();
    git_in(
        &forker,
        &[
            "-c",
            "user.name=F",
            "-c",
            "user.email=f@example.invalid",
            "commit",
            "-q",
            "-am",
            "feat(ledger): fork",
        ],
    );
    git_in(&forker, &["push", "-q", "origin", "HEAD:main"]);
    real.engine_sync().unwrap();
    let copy = w.copy();
    assert!(
        std::fs::read_to_string(copy.join("mappings.json"))
            .unwrap()
            .contains("fork")
    );
    let origin_was = w.origin_main();

    point(&w.origin);
    real.engine_sync().unwrap();
    assert_eq!(
        git_in(&copy, &["remote", "get-url", "origin"]),
        w.origin.display().to_string()
    );
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), origin_was);
    assert_eq!(
        w.origin_main(),
        origin_was,
        "the fork's row reached nothing"
    );
    assert!(!git_in(&w.origin, &["show", "main:mappings.json"]).contains("fork"));

    // NEGATIVE CONTROL: a row the fork lacks keeps the copy, and nothing is pushed.
    point(&fork);
    real.engine_sync().unwrap();
    std::fs::write(copy.join("mappings.json"), "{\"mappings\":{\"mine\":{}}}\n").unwrap();
    git_in(&copy, &["commit", "-q", "-am", "feat(ledger): mine"]);
    let held = git_in(&copy, &["rev-parse", "HEAD"]);
    let fork_was = git_in(&fork, &["rev-parse", "refs/heads/main"]);
    point(&w.origin);
    let refused = real.engine_sync().unwrap_err();
    assert!(
        refused
            .why
            .contains("holds changes or commits that origin lacks"),
        "{refused:?}"
    );
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), held);
    assert_eq!(w.origin_main(), origin_was);
    assert_eq!(git_in(&fork, &["rev-parse", "refs/heads/main"]), fork_was);
}

/// STEP 1 COUNTS THE ROWS THE RELEASE'S ENGINE HOLDS UNPUSHED: `pub publish` made R public and
/// committed its ledger row, whose push failed; the next run judges R published before its sync
/// delivers the row (a `--fix` or `--commit` must not move a public line), a dry run too, and
/// the sync then delivers it. NEGATIVE CONTROL: the origin's ledger alone has no row of R, so a
/// step 1 reading only the origin would judge R unpublished.
#[test]
fn step_one_counts_the_rows_the_releases_engine_holds_unpushed() {
    let w = World::new();
    w.real(false).engine_sync().unwrap();
    let copy = w.copy();
    let r = "d".repeat(40);
    std::fs::write(
        copy.join("mappings.json"),
        format!(
            "{{\"mappings\":{{\"aterm\":{{\"{r}\":{{\"verified_at\":\"2026-11-07T20:00:00Z\",\
             \"release_rev\":\"e2\"}}}}}}}}\n"
        ),
    )
    .unwrap();
    git_in(&copy, &["commit", "-q", "-am", "feat(ledger): aterm"]);
    assert!(
        !engine_rows_in(
            &git_in(&w.origin, &["show", "main:mappings.json"]),
            Some(&r)
        )
        .has,
        "NEGATIVE CONTROL: the origin's ledger has no row of R"
    );
    for dry in [true, false] {
        let mut next = w.real(dry);
        next.fetch_engine_origin().unwrap();
        let rows = next.engine_rows(Some(&r));
        assert!(rows.has, "dry={dry}: {rows:?}");
        assert_eq!(rows.rev.as_deref(), Some("e2"));
    }
    let mut next = w.real(false);
    next.fetch_engine_origin().unwrap();
    next.engine_sync().unwrap();
    assert!(next.engine_rows(Some(&r)).has);
    assert!(git_in(&w.origin, &["show", "main:mappings.json"]).contains(&r));
}
