// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A hung libtest-JSON probe is one completed red binary, not a reason to lose its peers.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use super::{
    Binary, CarryProof, Record, RunPlan, Watch, final_binary_cmd, run_planned_with_probe_window,
};
use crate::{Ctx, EnvSnapshot, Scope};

/// Set to a directory when this test binary is the isolated verifier run.
const STANDIN: &str = "ATERM_VERIFY_PROBE_HANG_STANDIN";
const FIFO: &str = "ATERM_VERIFY_PROBE_HANG_FIFO";
const INVOKED: &str = "ATERM_VERIFY_PROBE_HANG_INVOKED";
const PEER: &str = "ATERM_VERIFY_PROBE_HANG_PEER";
const PEER_PROBED: &str = "ATERM_VERIFY_PROBE_HANG_PEER_PROBED";
const ARMED: &str = "ATERM_VERIFY_PROBE_HANG_ARMED";

fn binary(spec: &str, executable: &Path, variables: &[(&str, &Path)]) -> Binary {
    // These shell stand-ins read only their declared fixture variables and cat.
    // Ambient Cargo/libtest switches must not silently choose another JSON mode
    // or become unrelated runtime-input evidence.
    let mut env: Vec<(OsString, OsString)> = vec![("PATH".into(), "/usr/bin:/bin".into())];
    env.extend(
        variables
            .iter()
            .map(|(key, value)| ((*key).into(), value.as_os_str().to_owned())),
    );
    Binary {
        header: format!("     Running {spec} ({})", executable.display()),
        spec: spec.to_owned(),
        record: Record {
            cwd: executable
                .parent()
                .expect("a fixture directory")
                .to_path_buf(),
            argv: vec![executable.as_os_str().to_owned()],
            env,
        },
        harness: true,
    }
}

/// The isolated run: its first binary writes one started case then blocks opening a FIFO. The
/// zero injected window proves it hung without a minute sleep; an ordinary halt-off run must then
/// execute the peer exactly once and retain the first binary's named hung red.
#[test]
fn standin_hung_probe_run() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    let fifo = dir.join("blocked.fifo");
    let fifo_c = CString::new(fifo.as_os_str().as_bytes()).expect("a NUL-free FIFO path");
    // SAFETY: `fifo_c` is a valid NUL-terminated path and the mode grants only this account.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let invoked = dir.join("probe-invoked");
    let peer_marker = dir.join("peer-ran");
    let peer_probed = dir.join("peer-probed");
    let stuck = dir.join("stuck-test");
    crate::standin_body(
        &stuck,
        "printf x >> \"$ATERM_VERIFY_PROBE_HANG_INVOKED\"\n\
         printf '%s\\n' '{\"type\":\"test\",\"event\":\"started\",\"name\":\"stuck_case\"}'\n\
         exec 3< \"$ATERM_VERIFY_PROBE_HANG_FIFO\"",
    );
    let peer = dir.join("peer-test");
    crate::standin_body(
        &peer,
        "case \" $* \" in\n\
           *\" --list \"*)\n\
             printf probe > \"$ATERM_VERIFY_PROBE_HANG_PEER_PROBED\"\n\
             printf '%s\\n' '{\"type\":\"suite\",\"event\":\"discovery\",\"test_count\":1}'\n\
             exit 0 ;;\n\
         esac\n\
         printf peer > \"$ATERM_VERIFY_PROBE_HANG_PEER\"\n\
         printf '%s\\n' \\
           '{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}' \\
           '{\"type\":\"test\",\"event\":\"started\",\"name\":\"peer_case\"}' \\
           '{\"type\":\"test\",\"event\":\"ok\",\"name\":\"peer_case\",\"exec_time\":0.01}' \\
           '{\"type\":\"suite\",\"event\":\"ok\",\"passed\":1,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.01}'",
    );
    let a = binary(
        "-p fixture --test stuck",
        &stuck,
        &[(FIFO, &fifo), (INVOKED, &invoked)],
    );
    let b = binary(
        "-p fixture --test peer",
        &peer,
        &[(PEER, &peer_marker), (PEER_PROBED, &peer_probed)],
    );
    let scratch = dir.join("scratch");
    std::fs::create_dir(&scratch).expect("the run's scratch");
    let armed = std::env::var_os(ARMED).as_deref() == Some(std::ffi::OsStr::new("1"));
    let mut ctx = Ctx::new(
        dir.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        scratch,
    )
    .with_halt(crate::halt_of(false, armed));
    ctx.hang_samples = Some(dir.join("samples"));
    let mut report = crate::ladder::Report::new("test");
    run_planned_with_probe_window(
        &ctx,
        &mut report,
        "test",
        RunPlan::fixture(vec![a, b], 1),
        crate::checks::Check::Test,
        Duration::ZERO,
    );
    std::fs::write(dir.join("report"), report.render()).expect("the report");
    std::fs::write(dir.join("done"), b"").expect("the completion marker");
}

struct Standin(aterm_uds::spawnfd::Child);

impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("atv-probe-hang-")
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch dir")
}

fn passing_harness(action: &str) -> String {
    format!(
        "case \" $* \" in\n\
           *\" --list \"*)\n\
             printf '%s\\n' '{{\"type\":\"suite\",\"event\":\"discovery\",\"test_count\":1}}'\n\
             exit 0 ;;\n\
         esac\n\
         {action}\n\
         printf '%s\\n' \\\n           '{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}}' \\\n           '{{\"type\":\"test\",\"event\":\"started\",\"name\":\"case\"}}' \\\n           '{{\"type\":\"test\",\"event\":\"ok\",\"name\":\"case\",\"exec_time\":0.01}}' \\\n           '{{\"type\":\"suite\",\"event\":\"ok\",\"passed\":1,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.01}}'"
    )
}

/// A hung probe is reported and never retried. A halt-off run executes its peer, while an armed
/// first-red run accounts for that peer as NOT RUN and never starts it. Each arm's outer one-minute
/// wait is a hang detector for the negative control: removing the production detector leaves the
/// FIFO reader blocked and the isolated child is killed on test failure.
#[test]
fn a_hung_probe_is_one_red_and_halt_off_continues_its_peers() {
    for armed in [false, true] {
        let dir = scratch();
        let events = dir.path().join("events");
        let mut command =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        command
            .args([
                "--exact",
                "testrun::probe_hang_tests::standin_hung_probe_run",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(STANDIN, dir.path())
            .env(ARMED, if armed { "1" } else { "0" })
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(std::fs::File::create(&events).expect("the events file"));
        let mut child = Standin(command.spawn().expect("the isolated verifier run"));
        let status = until_within(MUST_HAPPEN, "the probe-hang stand-in's end", || {
            child.0.try_wait().ok().flatten()
        });
        let report = std::fs::read_to_string(dir.path().join("report")).unwrap_or_default();
        let event_text = std::fs::read_to_string(&events).unwrap_or_default();
        let all = format!("{report}\n{event_text}");
        assert!(
            status.success() && dir.path().join("done").exists(),
            "armed={armed}: {status}\n{all}"
        );
        assert_eq!(
            dir.path().join("peer-ran").exists(),
            !armed,
            "armed={armed}: {all}"
        );
        assert_eq!(
            dir.path().join("peer-probed").exists(),
            !armed,
            "halt-off re-probes peers for named JSON cases; first-red probes none: {all}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("probe-invoked")).unwrap_or_default(),
            "x",
            "the hung binary is probed once and never dispatched again: {all}"
        );
        assert!(
            report.contains("hung") && report.contains("stuck_case"),
            "{all}"
        );
        assert!(
            event_text.contains("\"hung\":true")
                && event_text.contains("\"hung_cases\":[\"stuck_case\"]"),
            "{all}"
        );
        assert_eq!(
            report.contains("test binaries not run"),
            armed,
            "armed={armed}: {all}"
        );
    }
}

/// Carry is decided before the JSON probe starts a process. Two harnesses have complete prior
/// proofs and write markers on *any* invocation; the miss is the only executable the current hop
/// may probe or run. It mutates one candidate's declared fixture during dispatch. The unchanged
/// candidate stays carried without starting, while the drifted candidate becomes NOT RUN and
/// makes the check incomplete. The miss's own final proof names the fixture bytes dispatch read.
/// NEGATIVE CONTROLS: moving carry below `probe_json` creates a carried marker; omitting the final
/// fresh rehash records the drifted candidate OK; reusing pre-probe miss evidence records old bytes.
#[test]
fn a_carried_harness_is_never_started_by_the_json_probe() {
    for release in [true, false] {
        let dir = scratch();
        let root = dir.path().join("workspace");
        let package = root.join("crates/fixture");
        let target = root.join("target/debug/deps");
        let scratch = root.join("scratch");
        std::fs::create_dir_all(package.join("src")).unwrap();
        std::fs::create_dir_all(package.join("fixtures/miss")).unwrap();
        std::fs::create_dir_all(package.join("fixtures/drift")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/fixture\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        std::fs::write(root.join("trust.lock"), "build = 1\n").unwrap();
        std::fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n\
         [package.metadata.aterm.test-inputs]\n\
         bounded = true\ncontent-only = true\nread-only = true\npaths = []\nvolatile-environment = []\n\
         [package.metadata.aterm.test-inputs.targets.\"test:drift\"]\n\
         paths = [\"fixtures/drift\"]\n\
         [package.metadata.aterm.test-inputs.targets.\"test:miss\"]\n\
         paths = [\"fixtures/miss\"]\n",
    )
    .unwrap();
        std::fs::write(package.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        let fixture = package.join("fixtures/miss/value.txt");
        let drift_fixture = package.join("fixtures/drift/value.txt");
        let observed = root.join("miss-observed");
        std::fs::write(&fixture, "before").unwrap();
        std::fs::write(&drift_fixture, "before").unwrap();

        let carried_marker = root.join("carried-invoked");
        let drift_marker = root.join("drift-invoked");
        let miss_marker = root.join("miss-invoked");
        let events = root.join("events");
        let carried_exe = target.join("carried-0000000000000000");
        let drift_exe = target.join("drift-0000000000000000");
        let miss_exe = target.join("miss-0000000000000000");
        crate::standin_body(
            &carried_exe,
            &format!(
                "printf invoked >> \"$ATERM_CARRIED_PROBE_MARKER\"\n{}",
                passing_harness(":")
            ),
        );
        crate::standin_body(
            &drift_exe,
            "printf invoked >> \"$ATERM_DRIFT_PROBE_MARKER\"\n\
         printf '%s\\n' '{\"type\":\"suite\",\"event\":\"discovery\",\"test_count\":1}'",
        );
        crate::standin_body(
            &miss_exe,
            "case \" $* \" in\n\
           *\" --list \"*)\n\
             printf after > \"$ATERM_MISS_FIXTURE\"\n\
             printf probe >> \"$ATERM_MISS_PROBE_MARKER\"\n\
             printf '%s\\n' '{\"type\":\"suite\",\"event\":\"discovery\",\"test_count\":1}'\n\
             exit 0 ;;\n\
         esac\n\
         cat \"$ATERM_MISS_FIXTURE\" > \"$ATERM_MISS_OBSERVED\"\n\
         printf after > \"$ATERM_DRIFT_FIXTURE\"\n\
         printf run >> \"$ATERM_MISS_PROBE_MARKER\"\n\
         printf '%s\\n' \\
           '{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}' \\
           '{\"type\":\"test\",\"event\":\"started\",\"name\":\"peer_case\"}' \\
           '{\"type\":\"test\",\"event\":\"ok\",\"name\":\"peer_case\",\"exec_time\":0.01}' \\
           '{\"type\":\"suite\",\"event\":\"ok\",\"passed\":1,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.01}'",
        );
        let make = |spec: &str, executable: &Path, marker_name: &str, marker: &Path| {
            let mut binary = binary(spec, executable, &[(marker_name, marker)]);
            binary
                .record
                .env
                .push(("CARGO_PKG_NAME".into(), "fixture".into()));
            binary
                .record
                .env
                .push(("CARGO_MANIFEST_DIR".into(), package.as_os_str().to_owned()));
            binary
        };
        let carried = make(
            "-p fixture --test carried",
            &carried_exe,
            "ATERM_CARRIED_PROBE_MARKER",
            &carried_marker,
        );
        let drift = make(
            "-p fixture --test drift",
            &drift_exe,
            "ATERM_DRIFT_PROBE_MARKER",
            &drift_marker,
        );
        let mut miss = make(
            "-p fixture --test miss",
            &miss_exe,
            "ATERM_MISS_PROBE_MARKER",
            &miss_marker,
        );
        miss.record
            .env
            .push(("ATERM_MISS_FIXTURE".into(), fixture.as_os_str().to_owned()));
        miss.record.env.push((
            "ATERM_MISS_OBSERVED".into(),
            observed.as_os_str().to_owned(),
        ));
        miss.record.env.push((
            "ATERM_DRIFT_FIXTURE".into(),
            drift_fixture.as_os_str().to_owned(),
        ));
        let mut ctx = Ctx::new(
            root.clone(),
            Scope::workspace(),
            EnvSnapshot::default(),
            scratch.clone(),
        )
        .with_carry_context(Some("11".repeat(32)))
        .with_events(Some(crate::event_log::EventLog::open(&events).unwrap()));
        ctx.release = release;
        let builder = crate::test_content::EvidenceBuilder::new(
            ctx.carry_context.clone(),
            crate::report::running_driver().map(str::to_owned),
            scratch.clone(),
        );
        let command = final_binary_cmd(&ctx, &carried, 1, false, Watch::default());
        let command = crate::exec::finalize_exact_command(command, ctx.exec_env()).unwrap();
        let evidence = builder.evidence(&carried, &command);
        assert!(evidence.bounded(), "{evidence:?}");
        let first_started = Instant::now();
        let first = crate::exec::run(&command, ctx.exec_env());
        let first_execution = first_started.elapsed().as_secs_f64();
        assert!(first.ok && first.code == Some(0), "{first:?}");
        assert_eq!(std::fs::read_to_string(&carried_marker).unwrap(), "invoked");
        assert_eq!(
            evidence,
            crate::test_content::EvidenceBuilder::new(
                ctx.carry_context.clone(),
                crate::report::running_driver().map(str::to_owned),
                scratch.clone(),
            )
            .evidence(&carried, &command),
            "the actual first success leaves the complete input key unchanged"
        );
        let mut proof = CarryProof::passed(&carried.spec, &evidence, first_execution);
        proof.json = false;
        let drift_command = final_binary_cmd(&ctx, &drift, 1, false, Watch::default());
        let drift_command =
            crate::exec::finalize_exact_command(drift_command, ctx.exec_env()).unwrap();
        let drift_evidence = builder.evidence(&drift, &drift_command);
        assert!(drift_evidence.bounded(), "{drift_evidence:?}");
        let mut drift_proof = CarryProof::passed(&drift.spec, &drift_evidence, 1.0);
        drift_proof.json = false;
        let miss_command = final_binary_cmd(&ctx, &miss, 1, false, Watch::default());
        let miss_command =
            crate::exec::finalize_exact_command(miss_command, ctx.exec_env()).unwrap();
        let miss_before_probe = builder.evidence(&miss, &miss_command);
        assert!(miss_before_probe.bounded(), "{miss_before_probe:?}");
        let mut miss_proof = CarryProof::passed(&miss.spec, &miss_before_probe, 1.0);
        miss_proof.content_key = Some("33".repeat(32));
        ctx = ctx.with_passed_binaries(Some(
            [
                (carried.spec.clone(), proof),
                (drift.spec.clone(), drift_proof),
                (miss.spec.clone(), miss_proof),
            ]
            .into(),
        ));

        let mut report = crate::ladder::Report::new("test");
        run_planned_with_probe_window(
            &ctx,
            &mut report,
            "test",
            RunPlan::fixture(vec![carried.clone(), drift.clone(), miss.clone()], 1),
            crate::checks::Check::Test,
            Duration::from_secs(60),
        );
        let report = report.render();
        assert_eq!(
            std::fs::read_to_string(&carried_marker).unwrap(),
            "invoked",
            "the unchanged successful command must not even start for the next probe: {report}"
        );
        assert!(
            !drift_marker.exists(),
            "drifted carried harness started: {report}"
        );
        assert_eq!(
            std::fs::read_to_string(miss_marker).unwrap_or_default(),
            "proberun",
            "the miss is probed and then run exactly once: {report}"
        );
        assert!(
            report.contains("1 of 3 carried")
                && report.contains("1 run; 1 invalidated")
                && report.contains("NOT RUN test — 1 carried candidate(s) changed after selection")
                && report.contains(&drift.spec),
            "{report}"
        );
        assert_eq!(std::fs::read_to_string(&observed).unwrap(), "after");
        let after_command = final_binary_cmd(
            &ctx,
            &miss,
            1,
            false,
            Watch {
                json: true,
                ..Watch::default()
            },
        );
        let after_command =
            crate::exec::finalize_exact_command(after_command, ctx.exec_env()).unwrap();
        let after = crate::test_content::EvidenceBuilder::new(
            ctx.carry_context.clone(),
            crate::report::running_driver().map(str::to_owned),
            scratch.clone(),
        )
        .evidence(&miss, &after_command);
        assert!(after.bounded(), "{after:?}");
        assert_ne!(miss_before_probe.content_key, after.content_key);
        let event_text = std::fs::read_to_string(events).unwrap();
        assert!(
            event_text.contains(after.content_key.as_deref().unwrap()),
            "the miss event must name the post-probe fixture bytes: {event_text}"
        );
        let events: Vec<aterm_json::Value> = event_text
            .lines()
            .filter_map(|line| line.strip_prefix("verify-event: "))
            .map(|line| aterm_json::from_str(line).unwrap())
            .collect();
        let binary_events = |spec: &str| {
            events
                .iter()
                .filter(|event| {
                    event["event"].as_str() == Some("test-binary-finished")
                        && event["test"].as_str() == Some(spec)
                })
                .collect::<Vec<_>>()
        };
        let stable_events = binary_events(&carried.spec);
        assert_eq!(stable_events.len(), 1, "{stable_events:?}");
        let stable_event = stable_events[0];
        assert_eq!(stable_event["outcome"].as_str(), Some("ok"));
        assert_eq!(stable_event["carried"].as_bool(), Some(true));
        assert_eq!(stable_event["inputs_bounded"].as_bool(), Some(true));
        assert!(stable_event["exit_code"].is_null());
        assert!(stable_event["tests"].is_null());
        assert!(stable_event["test_counts"].is_null());
        assert!(stable_event["cpu_s"].is_null());
        assert_eq!(stable_event["execution_s"].as_f64(), Some(0.0));
        let drift_events = binary_events(&drift.spec);
        assert_eq!(
            drift_events.len(),
            1,
            "the drifted candidate gets exactly one final event, never an earlier OK: {drift_events:?}"
        );
        let drift_event = drift_events[0];
        assert_eq!(drift_event["outcome"].as_str(), Some("NOT RUN"));
        assert_eq!(drift_event["carried"].as_bool(), Some(false));
        assert_eq!(drift_event["inputs_bounded"].as_bool(), Some(false));
        assert!(drift_event["content_key"].is_null());
        assert!(
            drift_event["input_reason"]
                .as_str()
                .unwrap_or_default()
                .contains("changed after carry selection"),
            "{drift_event:?}"
        );

        let context_events = root.join("changed-context-events");
        let mut changed = Ctx::new(root, Scope::workspace(), EnvSnapshot::default(), scratch)
            .with_carry_context(Some("22".repeat(32)))
            .with_passed_binaries(ctx.passed_binaries.clone())
            .with_events(Some(
                crate::event_log::EventLog::open(&context_events).unwrap(),
            ));
        changed.release = release;
        run_planned_with_probe_window(
            &changed,
            &mut crate::ladder::Report::new("test"),
            "test",
            RunPlan::fixture(vec![carried.clone()], 1),
            crate::checks::Check::Test,
            Duration::from_secs(60),
        );
        assert_eq!(
            std::fs::read_to_string(&carried_marker).unwrap(),
            "invokedinvokedinvoked",
            "a changed caller context must probe and run the same executable afresh"
        );
        let context_events: Vec<aterm_json::Value> = std::fs::read_to_string(context_events)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("verify-event: "))
            .map(|line| aterm_json::from_str(line).unwrap())
            .filter(|event: &aterm_json::Value| {
                event["event"].as_str() == Some("test-binary-finished")
            })
            .collect();
        assert_eq!(context_events.len(), 1, "{context_events:?}");
        assert_eq!(context_events[0]["outcome"].as_str(), Some("ok"));
        assert_eq!(context_events[0]["exit_code"].as_i64(), Some(0));
        assert_eq!(context_events[0]["carried"].as_bool(), Some(false));
    }
}

/// Runtime-input evidence brackets the execution it describes. With one dispatch slot, the
/// second binary must hash a fixture changed by the first, not the bytes seen at probe end. A
/// successful binary which changes its own declared input violates `read-only = true` and keeps
/// no content key. NEGATIVE CONTROLS: hashing all misses before dispatch gives the reader the
/// old key; omitting the post-run rehash gives the self-mutator a bounded proof.
#[test]
fn miss_evidence_brackets_each_successful_execution() {
    for release in [true, false] {
        let dir = scratch();
        let root = dir.path().join("workspace");
        let package = root.join("crates/fixture");
        let target = root.join("target/debug/deps");
        let scratch = root.join("scratch");
        let reader_fixture = package.join("fixtures/reader/value.txt");
        let self_fixture = package.join("fixtures/self/value.txt");
        let observed = root.join("reader-observed");
        let events_path = root.join("events");
        std::fs::create_dir_all(package.join("src")).unwrap();
        std::fs::create_dir_all(reader_fixture.parent().unwrap()).unwrap();
        std::fs::create_dir_all(self_fixture.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/fixture\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        std::fs::write(root.join("trust.lock"), "build = 1\n").unwrap();
        std::fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n\
         [package.metadata.aterm.test-inputs]\n\
         bounded = true\ncontent-only = true\nread-only = true\npaths = []\nvolatile-environment = []\n\
         [package.metadata.aterm.test-inputs.targets.\"test:reader\"]\n\
         paths = [\"fixtures/reader\"]\n\
         [package.metadata.aterm.test-inputs.targets.\"test:self-mutator\"]\n\
         paths = [\"fixtures/self\"]\n",
    )
    .unwrap();
        std::fs::write(package.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        std::fs::write(&reader_fixture, "before").unwrap();
        std::fs::write(&self_fixture, "before").unwrap();

        let mutator_exe = target.join("mutator-0000000000000000");
        let reader_exe = target.join("reader-0000000000000000");
        let self_exe = target.join("self_mutator-0000000000000000");
        crate::standin_body(
            &mutator_exe,
            &passing_harness("printf after > \"$ATERM_READER_FIXTURE\""),
        );
        crate::standin_body(
            &reader_exe,
            &passing_harness("cat \"$ATERM_READER_FIXTURE\" > \"$ATERM_READER_OBSERVED\""),
        );
        crate::standin_body(
            &self_exe,
            &passing_harness("printf after > \"$ATERM_SELF_FIXTURE\""),
        );
        let make = |spec: &str, executable: &Path, variables: &[(&str, &Path)]| {
            let mut binary = binary(spec, executable, variables);
            binary
                .record
                .env
                .push(("CARGO_PKG_NAME".into(), "fixture".into()));
            binary
                .record
                .env
                .push(("CARGO_MANIFEST_DIR".into(), package.as_os_str().to_owned()));
            binary
        };
        let mutator = make(
            "-p fixture --test mutator",
            &mutator_exe,
            &[("ATERM_READER_FIXTURE", &reader_fixture)],
        );
        let reader = make(
            "-p fixture --test reader",
            &reader_exe,
            &[
                ("ATERM_READER_FIXTURE", &reader_fixture),
                ("ATERM_READER_OBSERVED", &observed),
            ],
        );
        let self_mutator = make(
            "-p fixture --test self-mutator",
            &self_exe,
            &[("ATERM_SELF_FIXTURE", &self_fixture)],
        );
        let mut ctx = Ctx::new(
            root,
            Scope::workspace(),
            EnvSnapshot::default(),
            scratch.clone(),
        )
        .with_carry_context(Some("11".repeat(32)))
        .with_events(Some(
            crate::event_log::EventLog::open(&events_path).unwrap(),
        ));
        ctx.release = release;
        let mut report = crate::ladder::Report::new("test");
        run_planned_with_probe_window(
            &ctx,
            &mut report,
            "test",
            RunPlan::fixture(vec![mutator, reader.clone(), self_mutator.clone()], 1),
            crate::checks::Check::Test,
            Duration::from_secs(60),
        );
        let report = report.render();
        assert!(
            report.contains(
                "NOT RUN test — runtime inputs changed while 1 successful test binary(s) ran"
            ) && report.contains(&self_mutator.spec),
            "a successful binary whose content proof drifted must not leave Test passed: {report}"
        );
        assert_eq!(
            std::fs::read_to_string(&observed).unwrap_or_default(),
            "after",
            "the declared order must put the mutator before the reader: {report}"
        );

        let reader_command = final_binary_cmd(
            &ctx,
            &reader,
            1,
            false,
            Watch {
                json: true,
                ..Watch::default()
            },
        );
        let reader_command =
            crate::exec::finalize_exact_command(reader_command, ctx.exec_env()).unwrap();
        let reader_after = crate::test_content::EvidenceBuilder::new(
            ctx.carry_context.clone(),
            crate::report::running_driver().map(str::to_owned),
            scratch,
        )
        .evidence(&reader, &reader_command);
        assert!(reader_after.bounded(), "{reader_after:?}");

        let events: Vec<aterm_json::Value> = std::fs::read_to_string(events_path)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("verify-event: "))
            .map(|line| aterm_json::from_str(line).unwrap())
            .collect();
        let event = |spec: &str| {
            events
                .iter()
                .find(|event| {
                    event["event"].as_str() == Some("test-binary-finished")
                        && event["test"].as_str() == Some(spec)
                })
                .unwrap_or_else(|| panic!("missing {spec:?} event: {events:?}"))
        };
        let reader_event = event(&reader.spec);
        assert_eq!(reader_event["inputs_bounded"].as_bool(), Some(true));
        assert_eq!(
            reader_event["content_key"].as_str(),
            reader_after.content_key.as_deref(),
            "the later reader hashes the fixture after the earlier binary changes it"
        );
        let self_event = event(&self_mutator.spec);
        assert_eq!(self_event["inputs_bounded"].as_bool(), Some(false));
        assert!(self_event["content_key"].is_null());
        assert!(
            self_event["input_reason"]
                .as_str()
                .unwrap_or_default()
                .contains("changed while the test binary ran"),
            "{self_event:?}"
        );
    }
}
