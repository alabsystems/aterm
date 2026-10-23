// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual child admission/report path with explicit RAM gate answers and virtual PID.
use super::*;
use crate::port::{Fake, RunFailed, Stat};
const PID: u32 = 4242;
const RUN: &str = "1-5";
const EXE: &str = "/A/agent/aterm";
const EFFECT: &str = "/synthetic-effect";

fn fixture() -> (Fake, Layout) {
    let sys = Fake::root();
    let dir = Stat {
        uid: 0,
        gid: 0,
        mode: 0o755,
        dir: true,
        symlink: false,
        len: 0,
    };
    for p in ["/", "/A", "/A/agent", "/A/state/runs/1-5"] {
        sys.stat_of(p, dir);
    }
    sys.stat_of(EXE, Stat { dir: false, ..dir });
    let envelope = Envelope {
        schema: crate::SCHEMA,
        host: "U".into(),
        host_name: "synthetic".into(),
        serial: 5,
        issued: 1,
        expires: 2,
        action: Action::Exec,
        disrupt: Disrupt::WhenIdle,
        idle_secs: 900,
        baseline: None,
        artifacts: vec![],
        exec: Some(envelope::Exec {
            argv: vec![EFFECT.into()],
            user: None,
            budget_secs: 30,
        }),
        upgrade: None,
        rekey: None,
    };
    let wire = format!(
        "-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n{}",
        envelope::body_text(&envelope).unwrap()
    );
    sys.file("/A/state/runs/1-5/envelope", wire.as_bytes())
        .stat_of(
            "/A/state/runs/1-5/envelope",
            Stat {
                dir: false,
                mode: 0o600,
                ..dir
            },
        )
        .file(
            "/A/state/run.lock",
            b"pid=4242 run=1-5 start=known_child_birth\n",
        );
    sys.on(
        "/usr/bin/env TZ=UTC0 /bin/ps -o lstart= -p 4242",
        Ran::ok("known child birth\n"),
    );
    sys.on(EFFECT, Ran::ok("one synthetic effect"));
    (sys, Layout { root: "/A".into() })
}

fn effects(sys: &Fake) -> usize {
    sys.calls()
        .iter()
        .filter(|call| call.as_str() == EFFECT)
        .count()
}

#[test]
fn a_committed_exact_job_runs_and_releases_only_its_own_holder() {
    let (sys, layout) = fixture();
    sys.start_answer(Ok(()));
    assert_eq!(
        run_job_at(&sys, &layout, Path::new(EXE), RUN, PID),
        ExitCode::SUCCESS
    );
    assert_eq!(effects(&sys), 1);
    let report: HostReport = aterm_json::from_slice(
        &sys.read_file(&layout.run_dir(RUN).join("report.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].id, "exec");
    assert_eq!(
        report.items[0].detail.as_deref(),
        Some("one synthetic effect")
    );
    assert!(state::holder(&sys, &layout).is_none());
    let old = state::Holder {
        pid: PID,
        run: RUN.into(),
        start: Some("known_child_birth".into()),
    };
    sys.file("/A/state/run.lock", b"pid=99 run=1-5 start=later\n");
    state::release_holder(&sys, &layout, &old).unwrap();
    assert_eq!(state::holder(&sys, &layout).unwrap().pid, 99);
}

#[test]
fn absent_start_or_mismatched_job_identity_refuses_before_host_effects() {
    let (sys, layout) = fixture();
    sys.start_answer(Ok(()));
    assert_eq!(
        run_job_at(&sys, &layout, Path::new(EXE), RUN, PID),
        ExitCode::SUCCESS
    );
    assert_eq!(effects(&sys), 1);
    let mut observations = Vec::new();
    for gate in [
        None,
        Some(RunFailed::TimedOut(crate::port::START_LIMIT)),
        Some(RunFailed::Refused(
            "synthetic EOF or invalid start byte".into(),
        )),
    ] {
        let (sys, layout) = fixture();
        if let Some(error) = gate {
            sys.start_answer(Err(error));
        }
        let code = run_job_at(&sys, &layout, Path::new(EXE), RUN, PID);
        observations.push((
            code == ExitCode::from(super::super::REFUSED),
            effects(&sys) == 0,
            sys.read_file(&layout.run_dir(RUN).join("report.json"))
                .is_err(),
            state::holder(&sys, &layout).is_some(),
            {
                let calls = sys.calls();
                calls.len() == 1 && calls[0] == "await start"
            },
        ));
    }
    for holder in [
        "pid=99 run=1-5 start=known_child_birth\n",
        "pid=4242 run=1-5 start=other_birth\n",
        "pid=4242 run=1-5\n",
    ] {
        let (sys, layout) = fixture();
        sys.file("/A/state/run.lock", holder.as_bytes());
        sys.start_answer(Ok(()));
        let code = run_job_at(&sys, &layout, Path::new(EXE), RUN, PID);
        let calls = sys.calls();
        observations.push((
            code == ExitCode::from(super::super::REFUSED),
            effects(&sys) == 0,
            sys.read_file(&layout.run_dir(RUN).join("report.json"))
                .is_err(),
            state::holder(&sys, &layout).is_some(),
            !calls
                .iter()
                .any(|call| call.contains("/usr/sbin/ioreg") || call.contains("/usr/bin/dscl")),
        ));
    }
    assert_eq!(observations, vec![(true, true, true, true, true); 6]);
}
