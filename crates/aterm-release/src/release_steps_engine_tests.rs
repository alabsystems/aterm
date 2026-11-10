// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release's own publication engine ([`aterm_verify::engine::engine_copy`]) against a stand-in
//! engine: an origin, the checkout other processes on the Mac align (`~/publication`), a peer
//! that lands engine code on the origin, and a release clone. The stand-in's `bin/pub` keeps the
//! two rules of the real engine's stage that decide this (`bin/pub`'s capture and
//! `revalidate_push_inputs`): HEAD is the origin's main when the stage starts, and a HEAD that
//! moved by anything but the data plane while it ran refuses the push.

use super::tests::{bind_native_engine_fixture, native_engine_fixture};
use super::*;

const ENGINE_URL: &str = "https://github.com/fixture/app.git";

const SHARED_NAME: &str = "Engine Owner";
const SHARED_EMAIL: &str = "owner@example.invalid";

#[test]
fn sync_rechecks_readiness_when_origin_replaces_the_engine() {
    let w = World::new();
    peer_lands(&w, "bin/pub", "#!/bin/sh\n# run_pre_promote_hook\nexit 0\n");
    let mut real = w.real(false);
    real.engine_sync().unwrap();
    assert!(real.engine_readiness().is_ok());
    peer_lands(&w, "bin/pub", "#!/bin/sh\nexit 0\n");
    // Negative control: removing sync's recheck reaches submodule sync instead.
    let refused = Backend::sync(&mut real).unwrap_err();
    assert!(refused.why.contains("no pre-promote seam"), "{refused:?}");
}

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
                 if [ -x '{before}' ]; then '{before}' || exit $?; fi\n\
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
                before = root.join("before-capture").display(),
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
        bind_native_engine_fixture(&shared, &origin);
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

    /// The release's own engine, where [`aterm_verify::engine::engine_copy`] puts it.
    fn copy(&self) -> PathBuf {
        aterm_verify::engine::engine_copy(&aterm_verify::engine::release_dir(&self.repo))
    }

    /// A real run's backend named the shared checkout (`dry`: a dry run's), its logs its own.
    fn real(&self, dry: bool) -> Real {
        let mut real = Real::new(&self.repo, Some(&self.shared), dry, false).unwrap();
        real.engine_origin = native_engine_fixture;
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
             git -C {checkout} fetch -q '{origin}' refs/heads/main:refs/remotes/origin/main\n\
             git -C {checkout} merge -q --ff-only origin/main\n",
            peer = self.peer.display(),
            origin = self.origin.display(),
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
         git -C \"$1\" fetch -q '{origin}' refs/heads/main:refs/remotes/origin/main\n\
         git -C \"$1\" merge -q --ff-only origin/main\n",
        peer = w.peer.display(),
        origin = w.origin.display(),
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
    assert_eq!(git_in(&copy, &["remote", "get-url", "origin"]), ENGINE_URL);
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

    // A later sync follows the same logical repository through a relocated fixture transport.
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
    bind_native_engine_fixture(&w.shared, &mirror);
    real.engine_sync().unwrap();
    assert_eq!(git_in(&copy, &["remote", "get-url", "origin"]), ENGINE_URL);
    assert_eq!(
        git_in(&copy, &["rev-parse", "HEAD"]),
        git_in(&mirror, &["rev-parse", "refs/heads/main"])
    );
    bind_native_engine_fixture(&w.shared, &w.origin);
    // Returning the fixture transport does not change the captured logical authority.
    real.engine_sync().unwrap();
    assert_eq!(git_in(&copy, &["remote", "get-url", "origin"]), ENGINE_URL);
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

/// EVERY CHILD OF A REAL RUN IS HANDED THE RELEASE'S ENGINE, AND A BACKEND NAMED THAT COPY RUNS
/// IT IN PLACE: `PUBLICATION_ENGINE` names the copy for each child (`pub` itself, `ship cut`,
/// which reads its ledger, `verify`, which reads its baseline), even before step 2 makes it; a
/// backend named the copy (by hand, or as a release older than the engine handover execs R's
/// cutter) keeps it and syncs it from its own origin.
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
    // A backend named the copy, as a release older than the handover execs R's cutter: it runs
    // it in place, from its own origin.
    w.peer_lands_code("pub-again");
    bind_native_engine_fixture(&copy, &w.origin);
    let mut again = Real::new(&w.repo, Some(&copy), false, false).unwrap();
    again.engine_origin = native_engine_fixture;
    again.logs = w.root.join("logs-again");
    std::fs::create_dir_all(&again.logs).unwrap();
    assert_eq!((&again.engine, &again.engine_source), (&copy, &copy));
    assert!(again.engine_unready().is_none());
    again.engine_sync().unwrap();
    assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), w.origin_main());
    assert_eq!(git_in(&copy, &["remote", "get-url", "origin"]), ENGINE_URL);
}

/// A DIFFERENT LOGICAL ORIGIN CANNOT RETARGET A CAPTURED RELEASE: the engine's source
/// is re-pointed at a foreign repository after the backend captured fixture/app. Both an
/// otherwise clean copy and one holding unpushed ledger rows stay untouched; neither origin
/// receives those rows. Physical fixture transport is declared separately from logical identity.
#[test]
fn a_foreign_origin_cannot_retarget_the_releases_captured_engine() {
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
    let mut real = w.real(false);
    real.engine_sync().unwrap();
    let copy = w.copy();
    let original_main = w.origin_main();
    let fork_main = git_in(&fork, &["rev-parse", "refs/heads/main"]);
    for holds_row in [false, true] {
        if holds_row {
            std::fs::write(copy.join("mappings.json"), "{\"mappings\":{\"mine\":{}}}\n").unwrap();
            git_in(&copy, &["commit", "-q", "-am", "feat(ledger): mine"]);
        }
        let held = git_in(&copy, &["rev-parse", "HEAD"]);
        let rows = std::fs::read_to_string(copy.join("mappings.json")).unwrap();
        git_in(
            &w.shared,
            &["config", "remote.fixture.url", &fork.display().to_string()],
        );
        git_in(
            &w.shared,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/foreign/app.git",
            ],
        );
        let refused = real.fetch_engine_origin().unwrap_err();
        assert!(!refused.why.is_empty(), "{refused:?}");
        let refused = real.engine_sync().unwrap_err();
        assert!(!refused.why.is_empty(), "{refused:?}");
        assert_eq!(git_in(&copy, &["rev-parse", "HEAD"]), held);
        assert_eq!(
            std::fs::read_to_string(copy.join("mappings.json")).unwrap(),
            rows
        );
        assert_eq!(git_in(&copy, &["status", "--porcelain"]), "");
        assert_eq!(w.origin_main(), original_main);
        assert_eq!(git_in(&fork, &["rev-parse", "refs/heads/main"]), fork_main);
        assert!(!git_in(&w.origin, &["show", "main:mappings.json"]).contains("fork"));
        assert!(
            !std::fs::read_to_string(copy.join("mappings.json"))
                .unwrap()
                .contains("fork")
        );
        assert!(!git_in(&w.origin, &["show", "main:mappings.json"]).contains("mine"));
        assert!(!git_in(&fork, &["show", "main:mappings.json"]).contains("mine"));
        bind_native_engine_fixture(&w.shared, &w.origin);
    }
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

/// STEP 1 COUNTS NO UNPUSHED ROW OF A COPY NAMING ANOTHER REPOSITORY: the release's engine holds
/// a committed ledger row of R its origin lacks, but names foreign/app, not the fixture/app this
/// release captured. That row is foreign/app's record, so step 1 (a dry run too) judges R by the
/// captured repository's ledger alone, which has none. NEGATIVE CONTROL: the same copy naming the
/// captured repository again has its row counted.
#[test]
fn step_one_counts_no_unpushed_row_of_a_copy_naming_another_repository() {
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
    let captured = git_in(&copy, &["remote", "get-url", "origin"]);
    let counted = |named: &str| {
        git_in(&copy, &["remote", "set-url", "origin", named]);
        [true, false].map(|dry| {
            let mut next = w.real(dry);
            next.fetch_engine_origin().unwrap();
            next.engine_rows(Some(&r)).has
        })
    };
    assert_eq!(
        counted("https://github.com/foreign/app.git"),
        [false, false]
    );
    assert_eq!(counted(&captured), [true, true], "NEGATIVE CONTROL");
}

/// The peer lands `path` holding `text` on the engine's origin.
fn peer_lands(w: &World, path: &str, text: &str) {
    git_in(&w.peer, &["pull", "-q", "--ff-only"]);
    let file = w.peer.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
    git_in(&w.peer, &["add", "-A"]);
    git_in(&w.peer, &["commit", "-q", "-m", &format!("land {path}")]);
    git_in(&w.peer, &["push", "-q", "origin", "HEAD:main"]);
}

/// A peer can land after qualify prepared the engine and scanners but before its export
/// branch starts. The raw stage is the negative control: its exact-origin guard refuses.
#[test]
fn the_export_admits_an_engine_origin_that_advanced_since_sync() {
    for path in ["verification-log.jsonl", "bin/new-engine-code"] {
        let w = World::new();
        let mut real = w.real(false);
        real.engine_sync().unwrap();
        real.prepare_scanner_tools(&w.prover).unwrap();
        peer_lands(&w, path, "peer landed before export\n");
        let admitted = w.origin_main();
        let (mut raw, _) = real
            .stage_command(&w.candidate.commit, &w.prover, StageTarget::Remote)
            .unwrap();
        let refused = real.output_forwarded(&mut raw).unwrap();
        assert!(!refused.status.success(), "the stale engine must refuse");
        assert!(
            String::from_utf8_lossy(&refused.stdout)
                .contains("does not exactly match queried origin/main")
        );

        let ran = Backend::branches(&mut real)
            .export(
                &w.candidate,
                &w.prover,
                StageTarget::Remote,
                &BranchStop::default(),
            )
            .unwrap();
        assert_eq!(ran.code, 0, "{path}: {ran:?}");
        assert!(
            git_in(
                &w.copy(),
                &["merge-base", "--is-ancestor", &admitted, "HEAD"]
            )
            .is_empty()
        );
        assert!(
            git_in(&w.origin, &["show", "main:verification-log.jsonl"])
                .contains("\"stage\":\"aterm\"")
        );
    }
}

/// Even a rewrite consisting only of records is not a fast-forward admission. Neither the
/// engine HEAD nor its origin may be repaired by merging the rewritten history at stage entry.
#[test]
fn the_export_refuses_a_rewritten_engine_origin_before_stage() {
    for divergent in [false, true] {
        let w = World::new();
        let base = w.origin_main();
        peer_lands(&w, "handoff/first", "first record\n");
        let mut real = w.real(false);
        real.engine_sync().unwrap();
        real.prepare_scanner_tools(&w.prover).unwrap();
        let held = git_in(&w.copy(), &["rev-parse", "HEAD"]);
        let rewritten = if divergent {
            git_in(&w.peer, &["checkout", "-q", "-b", "rewritten", &base]);
            std::fs::create_dir_all(w.peer.join("handoff")).unwrap();
            std::fs::write(w.peer.join("handoff/other"), "other record\n").unwrap();
            git_in(&w.peer, &["add", "-A"]);
            git_in(&w.peer, &["commit", "-qm", "divergent record"]);
            git_in(&w.peer, &["rev-parse", "HEAD"])
        } else {
            base
        };
        // Install the fixture's rewritten origin without any force-push transport.
        if divergent {
            git_in(
                &w.origin,
                &["fetch", "-q", &w.peer.display().to_string(), "rewritten"],
            );
        }
        git_in(&w.origin, &["update-ref", "refs/heads/main", &rewritten]);
        let refusal = Backend::branches(&mut real)
            .export(
                &w.candidate,
                &w.prover,
                StageTarget::Remote,
                &BranchStop::default(),
            )
            .unwrap_err();
        assert!(refusal.could_not_run, "{refusal:?}");
        assert!(refusal.why.contains("fast-forward"), "{refusal:?}");
        assert_eq!(git_in(&w.copy(), &["rev-parse", "HEAD"]), held);
        assert_eq!(w.origin_main(), rewritten);
        assert!(
            !real.side_log(Step::Qualify, Branch::Export.name()).exists(),
            "stage never starts"
        );
    }
}

/// A push after the last admission can still win the entry race. Preserve the engine's
/// refusal, but report no red of aterm's tree and prescribe an unchanged-candidate rerun.
#[test]
fn an_engine_push_after_export_admission_is_retryable_without_an_aterm_fix() {
    let w = World::new();
    let mut real = w.real(false);
    real.engine_sync().unwrap();
    real.prepare_scanner_tools(&w.prover).unwrap();
    write_exec(
        &w.root.join("before-capture"),
        &format!(
            "#!/bin/sh\nset -e\n{}",
            w.lands_and_aligns("last-instant", &format!("'{}'", w.peer.display()))
        ),
    );
    let refusal = Backend::branches(&mut real)
        .export(
            &w.candidate,
            &w.prover,
            StageTarget::Remote,
            &BranchStop::default(),
        )
        .unwrap_err();
    assert!(refusal.could_not_run, "{refusal:?}");
    assert!(
        refusal.why.contains("engine") && refusal.why.contains("race"),
        "{refusal:?}"
    );
    assert!(refusal.remedy.contains(RELEASE_COMMAND), "{refusal:?}");
    assert!(
        !refusal.remedy.contains("--fix") && !refusal.remedy.contains("land"),
        "{refusal:?}"
    );
    assert!(
        refusal
            .detail
            .iter()
            .any(|line| line.contains("does not exactly match queried origin/main")),
        "{refusal:?}"
    );
    assert_eq!(git_in(&w.copy(), &["rev-parse", "HEAD"]), w.origin_main());
    std::fs::remove_file(w.root.join("before-capture")).unwrap();

    // The engine also refuses code drift at its final pre-push revalidation.
    peer_lands(
        &w,
        "bin/pub",
        "#!/bin/sh\necho 'FAIL: publication engine origin/main changed during stage (non-data-plane paths: bin/pub-tool); refusing to push'\nexit 1\n",
    );
    let drift = Backend::branches(&mut real)
        .export(
            &w.candidate,
            &w.prover,
            StageTarget::Remote,
            &BranchStop::default(),
        )
        .unwrap_err();
    assert!(
        drift.could_not_run && drift.why.contains("engine origin race"),
        "{drift:?}"
    );

    // Negative control: an ordinary tree failure remains a stage verdict, not an engine race.
    peer_lands(
        &w,
        "bin/pub",
        "#!/bin/sh\necho 'FAIL: transforms failed'\nexit 1\n",
    );
    let ran = Backend::branches(&mut real)
        .export(
            &w.candidate,
            &w.prover,
            StageTarget::Remote,
            &BranchStop::default(),
        )
        .unwrap();
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.detail
            .iter()
            .any(|line| line.contains("transforms failed")),
        "{ran:?}"
    );
}

/// The peer renames `from` to `to` on the engine's origin.
fn peer_renames(w: &World, from: &str, to: &str) {
    git_in(&w.peer, &["pull", "-q", "--ff-only"]);
    std::fs::create_dir_all(w.peer.join(to).parent().unwrap()).unwrap();
    git_in(&w.peer, &["mv", from, to]);
    git_in(
        &w.peer,
        &["commit", "-q", "-m", &format!("move {from} to {to}")],
    );
    git_in(&w.peer, &["push", "-q", "origin", "HEAD:main"]);
}

/// THE ENGINE'S CODE IS WHAT ITS OWN STAGE GUARD CALLS CODE (`bin/pub`'s `engine_code_drift`):
/// records, the Rust port and the engine's prose are not; every other path is, a `*.md` under
/// `bin/` included. NEGATIVE CONTROL: the release's rule before named only the records, so
/// `docs/`, a top-level `*.md` and `crates/` were code.
#[test]
fn engine_code_is_what_the_engines_own_stage_guard_calls_code() {
    for path in [
        "mappings.json",
        "verification-log.jsonl",
        "handoff/v1/x.toml",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "crates/pubx/src/main.rs",
        "docs/RUNBOOK.md",
        "docs/measured/x.txt",
        "README.md",
        "AGENTS.md",
        "",
    ] {
        assert!(!engine_code(path), "{path} is not engine code");
    }
    for path in [
        "bin/pub",
        "bin/pub_ledger.py",
        "bin/NOTES.md",
        "registry/repos.toml",
        "registry/README.md",
        "keys/registry.toml",
        "baseline/forbidden-content.txt",
        "pins.json",
        "tests/test_pub.py",
        "README.txt",
    ] {
        assert!(engine_code(path), "{path} is engine code");
    }
}

/// A RESUMED RELEASE RESTAGES ONLY WHEN THE ENGINE'S CODE MOVED: the engine's main took prose
/// (`docs/`, a top-level `*.md`) and its Rust port since the release staged, as 101 of its 438
/// commits in the week to 2026-11-07 did, and the engine that staged is still the one that
/// promotes (`Same`). NEGATIVE CONTROL: the rule before (only records set aside) called that
/// `Moved` and threw the stage away. A move of `bin/` is `Moved`, naming the path; so is a
/// rename of engine code into `docs/`, which a diff with rename detection lists only at the path
/// it took (NEGATIVE CONTROL: without `--no-renames`, `Same`).
#[test]
fn a_resume_restages_only_for_a_move_of_the_engines_code() {
    let w = World::new();
    let mut real = w.real(false);
    real.engine_sync().unwrap();
    let staged = w.origin_main();
    peer_lands(&w, "docs/notes.md", "# notes\n");
    peer_lands(&w, "NOTES.md", "# top-level notes\n");
    peer_lands(&w, "crates/pubx/src/main.rs", "fn main() {}\n");
    peer_lands(&w, "Cargo.toml", "[workspace]\n");
    real.engine_sync().unwrap();
    assert_eq!(git_in(&w.copy(), &["rev-parse", "HEAD"]), w.origin_main());
    match Backend::engine_now(&mut real, &staged).unwrap() {
        EngineNow::Same(note) => assert!(note.contains("records, Rust port or prose"), "{note}"),
        other => panic!("only prose and the port moved: {other:?}"),
    }

    peer_lands(&w, "bin/pub-tool", "# engine code\n");
    real.engine_sync().unwrap();
    match Backend::engine_now(&mut real, &staged).unwrap() {
        EngineNow::Moved(why) => {
            assert!(why.contains("bin/pub-tool"), "{why}");
            assert!(!why.contains("docs/notes.md"), "prose is not named: {why}");
        }
        other => panic!("engine code moved: {other:?}"),
    }

    let staged = w.origin_main();
    peer_renames(&w, "bin/pub-tool", "docs/pub-tool");
    real.engine_sync().unwrap();
    match Backend::engine_now(&mut real, &staged).unwrap() {
        EngineNow::Moved(why) => assert!(why.contains("bin/pub-tool"), "{why}"),
        other => panic!("engine code left bin/: {other:?}"),
    }
}

/// A COMMIT THE RELEASE'S ENGINE HOLDS THAT RENAMES ENGINE CODE INTO ITS RECORDS IS KEPT, NOT
/// DELIVERED: the sync lists both paths of a rename, so `bin/pub-tool` moved to `handoff/` is
/// refused as code and the origin is untouched. NEGATIVE CONTROL: a listing with rename
/// detection names only `handoff/pub-tool`, a record, and the sync pushed the commit that
/// deleted engine code from the origin's main.
#[test]
fn a_held_rename_of_engine_code_into_the_records_is_never_delivered() {
    let w = World::new();
    peer_lands(&w, "bin/pub-tool", "# engine code\n");
    let real = w.real(false);
    real.engine_sync().unwrap();
    let copy = w.copy();
    std::fs::create_dir_all(copy.join("handoff")).unwrap();
    git_in(&copy, &["mv", "bin/pub-tool", "handoff/pub-tool"]);
    git_in(&copy, &["commit", "-q", "-m", "chore(audit): not a record"]);
    let held = git_in(&copy, &["rev-parse", "HEAD"]);
    let origin_was = w.origin_main();
    let refused = real.engine_sync().unwrap_err();
    assert!(
        refused
            .why
            .contains("change more than its records (bin/pub-tool)"),
        "{refused:?}"
    );
    assert_eq!(w.origin_main(), origin_was, "nothing was delivered");
    assert_eq!(
        git_in(&copy, &["rev-parse", "HEAD"]),
        held,
        "the commit is kept"
    );
}
