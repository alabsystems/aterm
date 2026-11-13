// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const ROLE: &str = "ATERM_BUILD_ADMIT_CAPTURE_ROLE";
const ROOT: &str = "ATERM_BUILD_ADMIT_CAPTURE_ROOT";
const PID: &str = "ATERM_BUILD_ADMIT_CAPTURE_PID";
const HANG: Duration = Duration::from_secs(60);

#[test]
fn standin() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    fs::write(
        root.join(format!("{role}.ready")),
        format!("{}\n", std::process::id()),
    )
    .unwrap();
    match role.as_str() {
        "success" => fs::write(root.join("sample"), std::env::var(PID).unwrap()).unwrap(),
        "empty" => {}
        "refused" => std::process::exit(13),
        "target" => loop {
            std::thread::sleep(POLL);
        },
        "blocked" => {
            while !root.join("sampler.release").exists() {
                std::thread::sleep(POLL);
            }
        }
        _ => panic!("unknown stand-in role"),
    }
}

fn standin_command(root: &Path, role: &str) -> Command {
    let mut cmd = command(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "build_admit::capture::tests::standin",
        "--test-threads=1",
    ])
    .env_clear()
    .env(ROLE, role)
    .env(ROOT, root);
    cmd
}

fn target(root: &Path) -> Sampler {
    let mut cmd = standin_command(root, "target");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .setsid(true);
    Sampler {
        child: Some(cmd.spawn().unwrap()),
    }
}

#[test]
fn sampler_receives_only_the_owned_targets_pid_and_never_reaps_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut target = target(root.path());
    let child = target.child.as_mut().unwrap();
    let pid = child.id();
    let receipt = capture_using(
        child,
        &root.path().join("sample"),
        Instant::now() + HANG,
        &|selected, _| {
            let mut cmd = standin_command(root.path(), "success");
            cmd.env(PID, selected.to_string());
            Ok(cmd)
        },
        &|| false,
    );
    let alive = !child.exited().unwrap();
    let selected = fs::read_to_string(root.path().join("sample")).unwrap();
    target.finish().unwrap();
    assert!(receipt.contains(": captured "), "{receipt}");
    assert_eq!(selected, pid.to_string());
    assert!(alive, "capture ended or reaped its target");
}

#[test]
fn refusal_and_empty_output_are_diagnostic_only() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut target = target(root.path());
    for role in ["empty", "refused", "permission"] {
        let receipt = capture_using(
            target.child.as_mut().unwrap(),
            &root.path().join("sample"),
            Instant::now() + HANG,
            &|_, _| {
                if role == "permission" {
                    return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                }
                Ok(standin_command(root.path(), role))
            },
            &|| false,
        );
        assert!(receipt.contains(": unavailable ("), "{receipt}");
    }
    let receipt = capture_using(
        target.child.as_mut().unwrap(),
        &root.path().join("sample"),
        Instant::now(),
        &|_, _| panic!("expired readiness must not start a sampler"),
        &|| false,
    );
    assert!(
        receipt.contains("readiness deadline elapsed before sampling"),
        "{receipt}"
    );
    let child = target.child.as_mut().unwrap();
    child.kill().unwrap();
    let started = Instant::now();
    while !child.exited().unwrap() {
        assert!(started.elapsed() < HANG, "hang detector: target exit");
        std::thread::sleep(POLL);
    }
    let receipt = capture_using(
        child,
        &root.path().join("sample"),
        Instant::now() + HANG,
        &|_, _| panic!("an exited target must not launch a sampler"),
        &|| false,
    );
    target.finish().unwrap();
    assert!(receipt.contains("owned target exited"));
}

#[test]
fn interruption_and_panic_reap_the_sampler_while_target_remains_owned() {
    for panic in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut target = target(root.path());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            capture_using(
                target.child.as_mut().unwrap(),
                &root.path().join("sample"),
                Instant::now() + HANG,
                &|_, _| Ok(standin_command(root.path(), "blocked")),
                &|| {
                    let ready = fs::read_to_string(root.path().join("blocked.ready"))
                        .ok()
                        .and_then(|text| {
                            text.strip_suffix('\n')
                                .and_then(|pid| pid.parse::<i32>().ok())
                        });
                    if ready.is_some() {
                        assert!(!panic, "stand-in for capture unwinding");
                        true
                    } else {
                        false
                    }
                },
            )
        }));
        let sampler_pid = fs::read_to_string(root.path().join("blocked.ready"))
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let sampler_gone = aterm_uds::process::process_presence(sampler_pid)
            == aterm_uds::process::ProcessPresence::Absent;
        let alive = !target.child.as_mut().unwrap().exited().unwrap();
        // A negative control with sampler Drop removed must release and finish its
        // stand-in before an assertion reports the leaked sampler.
        fs::write(root.path().join("sampler.release"), "go\n").unwrap();
        let by = Instant::now() + HANG;
        while aterm_uds::process::process_presence(sampler_pid)
            != aterm_uds::process::ProcessPresence::Absent
            && Instant::now() < by
        {
            std::thread::sleep(POLL);
        }
        target.finish().unwrap();
        assert_eq!(
            aterm_uds::process::process_presence(sampler_pid),
            aterm_uds::process::ProcessPresence::Absent,
            "fallback did not finish the sampler",
        );
        assert_eq!(result.is_err(), panic);
        assert!(sampler_gone && alive, "sampler leaked or target was reaped");
        if let Ok(receipt) = result {
            assert!(receipt.contains("interrupted during sampling"), "{receipt}");
        }
    }
}
