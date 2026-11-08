// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A hung libtest-JSON probe is one completed red binary, not a reason to lose its peers.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use super::{Binary, Record, RunPlan, run_planned_with_probe_window};
use crate::{Ctx, EnvSnapshot, Scope};

/// Set to a directory when this test binary is the isolated verifier run.
const STANDIN: &str = "ATERM_VERIFY_PROBE_HANG_STANDIN";
const FIFO: &str = "ATERM_VERIFY_PROBE_HANG_FIFO";
const INVOKED: &str = "ATERM_VERIFY_PROBE_HANG_INVOKED";
const PEER: &str = "ATERM_VERIFY_PROBE_HANG_PEER";
const PEER_PROBED: &str = "ATERM_VERIFY_PROBE_HANG_PEER_PROBED";
const ARMED: &str = "ATERM_VERIFY_PROBE_HANG_ARMED";

fn binary(spec: &str, executable: &Path, variables: &[(&str, &Path)]) -> Binary {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| key != STANDIN)
        .collect();
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
        .tempdir()
        .expect("a scratch dir")
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
