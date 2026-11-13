// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::path::Path;
use std::time::Duration;

use super::{
    ChildOut, ChildSpec, RunError, ScriptedChild, classify_failure, classify_progress_failure, run,
};
use crate::error::{Kind, SideEffect, Sub};
use crate::fx::Fx;

#[cfg(unix)]
const ROUTED_ENV_CASE: &str = "AGH_ROUTED_CHILD_ENV_CASE";
#[cfg(unix)]
const ROUTED_ENV_MARKER: &str = "AGH_ROUTED_CHILD_ENV_MARKER";
#[cfg(unix)]
const ROUTED_ENV_TEST: &str =
    "fx::child::tests::a_routed_git_child_gets_only_its_explicit_environment";

/// A child with routed Git's explicit authentication environment is actually re-executed
/// beneath a parent holding both ambient token variables. It sees neither, and it sees
/// prompting disabled. NEGATIVE CONTROL: removing `run`'s `env_clear` makes the probe
/// inherit the planted variables and fail.
#[cfg(unix)]
#[test]
fn a_routed_git_child_gets_only_its_explicit_environment() {
    match std::env::var(ROUTED_ENV_CASE).as_deref() {
        Ok("probe") => {
            assert!(std::env::var_os("GH_TOKEN").is_none());
            assert!(std::env::var_os("GITHUB_TOKEN").is_none());
            assert_eq!(
                std::env::var_os("GIT_TERMINAL_PROMPT").as_deref(),
                Some(std::ffi::OsStr::new("0"))
            );
            let marker = std::env::var_os(ROUTED_ENV_MARKER).expect("the marker path");
            std::fs::write(marker, b"exact environment").expect("write the marker");
            return;
        }
        Ok("parent") => {
            assert!(std::env::var_os("GH_TOKEN").is_some());
            assert!(std::env::var_os("GITHUB_TOKEN").is_some());
            let marker = std::env::var_os(ROUTED_ENV_MARKER).expect("the marker path");
            let exe = std::env::current_exe().expect("the test binary");
            let out = run(&ChildSpec::new(&exe)
                .arg("--exact")
                .arg(ROUTED_ENV_TEST)
                .arg("--test-threads=1")
                .arg("--nocapture")
                .env(ROUTED_ENV_CASE, "probe")
                .env(ROUTED_ENV_MARKER, marker)
                .env("GIT_TERMINAL_PROMPT", "0"))
            .expect("the exact-environment probe ran");
            assert_eq!(out.code, Some(0), "{out:?}");
            return;
        }
        Err(_) => {}
        other => panic!("unknown routed child environment case {other:?}"),
    }

    let root = aterm_tempfile::tempdir().expect("scratch");
    let marker = root.path().join("exact-environment");
    let mut parent =
        aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary's own path"));
    parent
        .args([
            "--exact",
            ROUTED_ENV_TEST,
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ROUTED_ENV_CASE, "parent")
        .env(ROUTED_ENV_MARKER, &marker)
        .env("GH_TOKEN", "planted-dead-token")
        .env("GITHUB_TOKEN", "planted-dead-token");
    let out = aterm_uds::deadline::output_within(&mut parent, Duration::from_secs(60))
        .expect("the parent finished within the hang detector");
    assert!(out.status.success(), "the parent failed");
    assert_eq!(
        std::fs::read(marker).expect("the probe's marker"),
        b"exact environment"
    );
}

/// Successful dispatch permits effects even if the output reader never starts. The
/// other collector failures keep their previous classifications and diagnostic stages.
#[test]
fn collector_failures_preserve_dispatch_and_side_effect_classification() {
    let spec = ChildSpec::new(Path::new("/fixture/git")).budget(Duration::from_secs(73));
    let cause = || std::io::Error::other("collector-classification-marker");
    for (error, effect, why) in [
        (
            RunError::Spawn(cause()),
            SideEffect::None,
            "/fixture/git could not start: collector-classification-marker",
        ),
        (
            RunError::Capture(cause()),
            SideEffect::Unknown,
            "/fixture/git was dispatched but output capture failed: collector-classification-marker",
        ),
        (
            RunError::Wait(cause()),
            SideEffect::Unknown,
            "/fixture/git could not be waited for: collector-classification-marker",
        ),
        (
            RunError::TimedOut,
            SideEffect::Unknown,
            "/fixture/git did not finish in 73s",
        ),
    ] {
        let data = classify_failure(&spec, error).into_data();
        assert_eq!(
            (data.kind, data.sub, data.side_effect),
            (Kind::CouldNotRun, Sub::Local, effect)
        );
        assert_eq!(data.why, why);
    }
}

/// A progress stall is a post-dispatch unknown-effect result, and its reason preserves both
/// the evidence window and the credential-free native operation name.
#[test]
fn progress_stall_names_its_evidence_and_wait() {
    let spec = ChildSpec::new(Path::new("/fixture/git"));
    let error = aterm_uds::deadline::ProgressError::Stalled {
        window: Duration::from_secs(60),
        waiting_on: "git fetch-batch fixture/app, step fetch".into(),
    };
    let data = classify_progress_failure(&spec, error).into_data();
    assert_eq!(
        (data.kind, data.sub, data.side_effect),
        (Kind::CouldNotRun, Sub::Local, SideEffect::Unknown)
    );
    for words in [
        "no stdout/stderr bytes",
        "descendant tree made no CPU or I/O progress",
        "60s",
        "git fetch-batch fixture/app, step fetch",
    ] {
        assert!(data.why.contains(words), "{}", data.why);
    }
}

/// `Instant + Duration::MAX` panics; the run answers could_not_run instead, before anything
/// is spawned, so nothing was written. The child is one that runs and exits 0 under a
/// budget the clock holds (NEGATIVE CONTROL, first), so only the budget's refusal can make
/// the second run fail.
#[cfg(unix)]
#[test]
fn a_budget_past_the_clock_is_could_not_run() {
    let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
    let runs = ChildSpec::new(&sleep)
        .arg("0")
        .budget(Duration::from_secs(60));
    assert_eq!(run(&runs).expect("it runs").code, Some(0));
    let spec = ChildSpec::new(&sleep).arg("0").budget(Duration::MAX);
    let err = run(&spec).expect_err("refused").into_data();
    assert_eq!(
        (err.kind, err.sub, err.side_effect),
        (Kind::CouldNotRun, Sub::Local, SideEffect::None)
    );
}

#[cfg(unix)]
#[test]
fn explicit_child_input_is_bounded_and_never_inherited_or_debug_printed() {
    let cat = ChildSpec::new(Path::new("/bin/cat"));
    assert!(run(&cat).expect("null input").stdout.is_empty());
    let mut input = vec![b'x'; 128 * 1024];
    input.extend_from_slice(b"input-content-marker");
    let spec = cat.clone().input(&input);
    assert!(!format!("{spec:?}").contains("input-content-marker"));
    assert_eq!(
        run(&spec).expect("input larger than a pipe buffer").stdout,
        input
    );
    let oversized = cat.input(&vec![0; 1024 * 1024 + 1]);
    let error = run(&oversized).expect_err("bounded before spawn");
    assert_eq!(error.side_effect, SideEffect::None);
}

/// A child that started and was killed at its bound may have acted first (a git push may
/// have reached its remote): could_not_run, with an unknown side effect. NEGATIVE CONTROL:
/// the refusals before a spawn carry none (the tests beside this one).
#[cfg(unix)]
#[test]
fn a_child_killed_at_its_bound_may_have_acted() {
    let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
    let spec = ChildSpec::new(&sleep)
        .arg("60")
        .budget(Duration::from_millis(200));
    let err = run(&spec).expect_err("killed at its bound").into_data();
    assert_eq!(
        (err.kind, err.sub, err.side_effect),
        (Kind::CouldNotRun, Sub::Local, SideEffect::Unknown)
    );
}

#[test]
fn explicit_git_stop_before_dispatch_starts_nothing() {
    let missing = ChildSpec::new(Path::new("/missing/agh-stop-must-not-spawn"));
    let error = super::run_unless(&missing, &mut || true)
        .expect_err("stop precedes dispatch")
        .into_data();
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Interrupted, SideEffect::None)
    );
}

#[cfg(unix)]
fn original_process_retired(
    token: aterm_uds::process::Identity,
    exited_is_retired: bool,
) -> (
    bool,
    aterm_uds::process::ProcessPresence,
    Option<aterm_uds::process::Identity>,
) {
    use aterm_uds::process::ProcessPresence;

    // Read before the identity: `token`, born earlier and still holding its pid after,
    // held it at this read too.
    let killed = kill_pending(token.pid);
    let identity = aterm_uds::process::read_identity(token.pid);
    let presence = aterm_uds::process::process_presence(token.pid);
    let retired = identity.is_some_and(|current| current != token)
        || match presence {
            ProcessPresence::Absent => true,
            ProcessPresence::Exited => exited_is_retired,
            ProcessPresence::Present => exited_is_retired && killed && identity == Some(token),
            ProcessPresence::Unknown => false,
        };
    (retired, presence, identity)
}

/// Whether `SIGKILL` is pending for `pid`, which then runs none of its code again: Linux
/// lists a killed process as running until the scheduler runs its exit, which on a loaded
/// machine is after the stop returns. A group's `SIGKILL` sets the process-wide mask
/// (`ShdPnd`), which only the reap clears. A thread's own mask (`SigPnd`) proves nothing:
/// an exec from another thread sets it on the old leader while the process runs on. A
/// signal the kernel drops sets no bit: one to a namespace's init from inside it, or to a
/// process already exiting.
#[cfg(unix)]
fn kill_pending(pid: i32) -> bool {
    const SIGKILL: u64 = 1 << (9 - 1);
    cfg!(any(target_os = "linux", target_os = "android"))
        && aterm_uds::process::read_proc_text(Path::new(&format!("/proc/{pid}/status"))).is_ok_and(
            |status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("ShdPnd:"))
                    .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
                    .is_some_and(|mask| mask & SIGKILL != 0)
            },
        )
}

/// The owner positively reaps its root and makes every group member unable to run before
/// return. An orphaned killed descendant may still be a zombie until its adopter reaps it,
/// or on Linux still be listed with its `SIGKILL` pending ([`kill_pending`]).
/// NEGATIVE CONTROL: killing only the root leaves the same descendant identity `Present`;
/// omitting the owning wait leaves the root `Exited`; broad signalling kills the bystander.
#[cfg(unix)]
#[test]
fn explicit_git_stop_retires_wall_and_progress_children_without_touching_a_bystander() {
    use std::time::Instant;
    struct Bystander(aterm_uds::spawnfd::Child);
    impl Drop for Bystander {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut bystander = Bystander(
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("600")
            .spawn()
            .expect("owned bystander"),
    );
    for progress in [false, true] {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let marker = dir.path().join("pid");
        let mut spec = ChildSpec::new(Path::new("/bin/sh"))
            .arg("-c")
            .arg("/bin/sh -c 'while test ! -f \"$0.release\"; do /bin/sleep 0.01; done' \"$0\" & printf '%s %s\\n' \"$$\" \"$!\" > \"$0\"; wait")
            .arg(marker.as_os_str())
            .budget(Duration::from_secs(60));
        if progress {
            spec = spec.progressing(Duration::from_secs(60), "owned stop fixture");
        }
        let mut identities = None;
        let error = super::run_unless(&spec, &mut || {
            if identities.is_some() {
                return true;
            }
            let Ok(text) = std::fs::read_to_string(&marker) else {
                return false;
            };
            if !text.ends_with('\n') {
                return false;
            }
            let pids: Vec<i32> = text
                .split_whitespace()
                .map(|pid| pid.parse().expect("pid"))
                .collect();
            assert_eq!(pids.len(), 2);
            let Some(root) = aterm_uds::process::read_identity(pids[0]) else {
                return false;
            };
            let Some(descendant) = aterm_uds::process::read_identity(pids[1]) else {
                return false;
            };
            identities = Some([root, descendant]);
            true
        })
        .expect_err("explicit stop")
        .into_data();
        assert_eq!(
            (error.kind, error.side_effect),
            (Kind::Interrupted, SideEffect::Unknown)
        );
        let [root, descendant] = identities.expect("actual children started");
        let root_at_return = original_process_retired(root, false);
        let descendant_at_return = original_process_retired(descendant, true);
        std::fs::write(marker.with_extension("release"), b"release").expect("release fixture");
        let until = Instant::now() + Duration::from_secs(60);
        while !original_process_retired(descendant, true).0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let descendant_after_cleanup = original_process_retired(descendant, true);
        assert!(
            descendant_after_cleanup.0,
            "fixture descendant could no longer run: {descendant_after_cleanup:?}"
        );
        assert!(
            root_at_return.0,
            "owned root was positively reaped before return: {root_at_return:?}"
        );
        assert!(
            descendant_at_return.0,
            "owned descendant could no longer run before return: {descendant_at_return:?}"
        );
        assert!(bystander.0.try_wait().expect("bystander wait").is_none());
    }
}

/// A child's `Debug` never holds its streams: `gh auth token`'s stdout is a token.
#[test]
fn a_childs_debug_names_its_stream_lengths_not_their_bytes() {
    let out = ChildOut {
        code: Some(0),
        stdout: b"agh-child-stdout-marker".to_vec(),
        stderr: b"agh-child-stderr-marker".to_vec(),
    };
    let shown = format!("{out:?} {:?}", Ok::<_, ()>(out.clone()));
    assert!(!shown.contains("marker"), "{shown}");
    assert!(shown.contains("23 bytes"), "{shown}");
}

/// `git` follows `child`'s rule: a spec `child::check` refuses is not logged.
#[test]
fn a_relative_git_is_refused_and_not_logged_under_simulation() {
    let fx = Fx::simulated(1_760_000_000);
    let spec = ChildSpec::new(Path::new("git")).arg("status");
    let err = fx.git(&spec, b"").expect_err("refused").into_data();
    assert_eq!(
        (err.kind, err.sub, err.side_effect),
        (Kind::CouldNotRun, Sub::Local, SideEffect::None)
    );
    assert!(fx.children_run().is_empty());
}

#[cfg(unix)]
#[test]
fn git_child_observation_preserves_config_input_exits_and_errors() {
    use super::ConfigQuery;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    let git = aterm_uds::fixture_git::git();
    let mut base = ChildSpec::new(Path::new(git.get_program()));
    base.env.extend(
        git.get_envs()
            .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned()))),
    );
    let query = |bytes: &[u8], query: ConfigQuery| {
        let mut spec = base.clone().config_input(bytes, query);
        spec.args = query.args().iter().map(|arg| (*arg).into()).collect();
        spec
    };
    let specs = [
        base.clone().arg("--version"),
        query(
            b"[user]\nname = private-config-marker\n",
            ConfigQuery::Names { read_only: false },
        ),
        query(
            b"[core]\nrepositoryformatversion = 0\n",
            ConfigQuery::WorktreeFlag,
        ),
        query(
            b"[broken\nprivate-config-marker\n",
            ConfigQuery::Names { read_only: false },
        ),
    ];
    let spans = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&spans);
    let fx = Fx::simulated(1_760_000_000).with_git_child_observer_for_tests(move |span| {
        captured.lock().unwrap().push(span);
    });
    for (ordinal, spec) in specs.iter().enumerate() {
        let live = Fx::live().git(spec, b"").unwrap();
        let inactive = Fx::simulated(1_760_000_000).git(spec, b"").unwrap();
        let began = Instant::now();
        let observed = fx.git(spec, b"").unwrap();
        let finished = Instant::now();
        assert_eq!(observed, live);
        assert_eq!(observed, inactive);
        assert_eq!(observed.code == Some(0), ordinal < 2);
        assert!(!String::from_utf8_lossy(&observed.stdout).contains("private-config-marker"));
        assert!(!String::from_utf8_lossy(&observed.stderr).contains("private-config-marker"));
        let spans = spans.lock().unwrap();
        assert_eq!(spans.len(), ordinal + 1);
        let span = spans.last().unwrap();
        assert_eq!(
            (span.ordinal, span.code, span.error),
            (ordinal, observed.code, None)
        );
        assert!(
            began <= span.started && span.started <= span.finished && span.finished <= finished
        );
    }
    let refused = ChildSpec::new(Path::new("git")).arg("status");
    let error = fx.git(&refused, b"").unwrap_err();
    assert_eq!(error.side_effect, SideEffect::None);
    assert_eq!(spans.lock().unwrap().len(), 4);
    assert_eq!(fx.children_run().len(), 4);
    let root = aterm_tempfile::tempdir().unwrap();
    let generic = ChildSpec::new(&root.path().join("scripted-child"));
    let fx = fx.with_children(vec![ScriptedChild {
        program: generic.program.clone(),
        args: Vec::new(),
        out: ChildOut {
            code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    }]);
    fx.child(&generic).unwrap();
    assert_eq!(
        spans.lock().unwrap().len(),
        4,
        "other child door is not observed"
    );
    assert_eq!(fx.children_run().len(), 5);
    let absent = ChildSpec::new(&root.path().join("absent-git"));
    let live = Fx::live().git(&absent, b"").unwrap_err().into_data();
    let observed = fx.git(&absent, b"").unwrap_err().into_data();
    assert_eq!(observed, live);
    let spans = spans.lock().unwrap();
    assert_eq!(spans.len(), 5);
    assert_eq!(
        (spans[4].ordinal, spans[4].code, spans[4].error),
        (5, None, Some(Kind::CouldNotRun))
    );
    assert!(spans.windows(2).all(|p| p[0].finished <= p[1].started));
}

#[cfg(unix)]
#[test]
fn git_child_observation_refuses_other_modes_and_replacement() {
    use crate::fx::tape::{TapeEndpoints, TapeHeader, TapeMode};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("record.atape");
    let header = TapeHeader {
        agh_version: crate::version().to_owned(),
        api_version: crate::API_VERSION.to_owned(),
        argv: vec!["fixture".into()],
        mode: TapeMode::Anonymous,
        endpoints: TapeEndpoints::Loopback,
    };
    let reject = |fx: Fx| {
        assert!(
            catch_unwind(AssertUnwindSafe(
                || fx.with_git_child_observer_for_tests(|_| {})
            ))
            .is_err()
        )
    };
    reject(Fx::live());
    reject(Fx::record_for_tests(&path, &header).unwrap());
    reject(Fx::replay_for_tests(&path).unwrap().0);
    reject(
        Fx::rerecord_for_tests(&path, &root.path().join("new.atape"))
            .unwrap()
            .0,
    );
    reject(Fx::simulated(1).with_git_child_observer_for_tests(|_| {}));
}

#[test]
fn a_relative_program_is_refused_under_simulation_as_it_is_live() {
    let spec = ChildSpec::new(Path::new("gh")).arg("auth");
    let fx = Fx::simulated(1_760_000_000).with_children(vec![ScriptedChild {
        program: Path::new("gh").to_path_buf(),
        args: vec!["auth".to_string()],
        out: ChildOut {
            code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    }]);
    let simulated = fx.child(&spec).expect_err("refused").into_data();
    let live = run(&spec).expect_err("refused").into_data();
    assert_eq!(simulated, live);
    assert_eq!(
        (live.kind, live.sub, live.side_effect),
        (Kind::CouldNotRun, Sub::Local, SideEffect::None)
    );
    assert!(fx.children_run().is_empty());
}

#[test]
#[should_panic(expected = "Fx::with_children scripts only a simulated Fx")]
fn scripting_children_on_a_live_fx_panics() {
    let _ = Fx::live().with_children(Vec::new());
}

#[cfg(unix)]
#[test]
fn source_inventory_spools_more_than_tail_capacity_with_bounded_records() {
    use std::io::{BufRead as _, Write as _};
    let root = aterm_tempfile::Builder::new()
        .prefix("agh-inventory-")
        .tempdir()
        .unwrap();
    let input = root.path().join("input");
    let row = format!("{}\n", "1".repeat(40));
    let mut file = std::fs::File::create(&input).unwrap();
    for _ in 0..66_023 {
        file.write_all(row.as_bytes()).unwrap();
    }
    drop(file);
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    for progress in [false, true] {
        let destination = root.path().join(if progress { "progress" } else { "wall" });
        let mut spec = ChildSpec::new(&cat)
            .arg(input.as_os_str())
            .budget(Duration::from_secs(60));
        if progress {
            spec = spec.progressing(Duration::from_secs(60), "complete source inventory");
        }
        spec.stdout_records = Some(destination.clone());
        let out = run(&spec).unwrap();
        assert_eq!(out.code, Some(0));
        assert!(
            out.stdout.is_empty(),
            "the complete inventory is never a heap/tail answer"
        );
        assert_eq!(std::fs::metadata(&destination).unwrap().len(), 66_023 * 41);
        let mut lines = 0;
        for line in std::io::BufReader::new(std::fs::File::open(destination).unwrap()).lines() {
            assert_eq!(line.unwrap(), "1".repeat(40));
            lines += 1;
        }
        assert_eq!(lines, 66_023);
    }
    for bytes in [
        b"unterminated".as_slice(),
        b"this record has more than forty two bytes and must be rejected\n".as_slice(),
    ] {
        std::fs::write(&input, bytes).unwrap();
        let destination = root.path().join("refused");
        let mut spec = ChildSpec::new(&cat).arg(input.as_os_str());
        spec.stdout_records = Some(destination.clone());
        assert!(
            run(&spec).is_err(),
            "partial/oversized native records cannot become complete DATA"
        );
        std::fs::remove_file(destination).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn source_inventory_stop_retires_the_actual_writer_before_returning() {
    let root = aterm_tempfile::Builder::new()
        .prefix("agh-inventory-stop-")
        .tempdir()
        .unwrap();
    let marker = root.path().join("started");
    let sh = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let mut spec = ChildSpec::new(&sh).arg("-c")
        .arg("printf '1111111111111111111111111111111111111111\\n'; printf '%s' $$ > \"$1\"; /bin/sleep 60")
        .arg("sh").arg(marker.as_os_str()).budget(Duration::from_secs(60));
    spec.stdout_records = Some(root.path().join("records"));
    let error = super::run_unless(&spec, &mut || marker.exists()).unwrap_err();
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Interrupted, SideEffect::Unknown)
    );
    let pid: i32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
    assert_eq!(
        aterm_uds::process::process_presence(pid),
        aterm_uds::process::ProcessPresence::Absent
    );
}
