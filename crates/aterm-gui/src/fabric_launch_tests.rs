// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::app_config::{Config, FabricCommand, FabricConfig};

fn with_command(cmd: Option<&str>) -> Config {
    with_argv(cmd.map(aterm_types::fabric_command::split_line))
}

fn with_argv(argv: Option<Vec<String>>) -> Config {
    Config {
        fabric: Some(FabricConfig {
            command: argv.map(FabricCommand),
            hold_on_bridge_lost: None,
        }),
        ..Config::default()
    }
}

fn parsed(text: &str) -> Result<Config, String> {
    Config::parse(text).map_err(|e| e.to_string())
}

/// A [`Told`] that tells the process-wide link nothing, for a test about the lanes alone.
fn untold() -> Told {
    Told {
        attached: |_| {},
        lost: Box::new(|_| {}),
    }
}

fn true_program() -> String {
    ["/usr/bin/true", "/bin/true"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("a `true` binary")
        .to_string()
}

/// The bridge's argv is aterm.toml's `[fabric] command` (cap paths, broker addresses): the
/// started line names its program's file name and counts its arguments (DESIGN §3.6).
#[test]
fn the_started_line_names_the_program_and_counts_its_arguments() {
    let argv = vec![true_program(), "QZP7E7E1".to_string()];
    let launches = std::cell::Cell::new(0_u32);
    let logged = crate::test_log::logged_while(|| {
        supervise_with(
            &argv,
            |argv| {
                launches.set(launches.get() + 1);
                launch_serving(argv, |_, _, _| true, untold())
            },
            || launches.get() > 0,
            |_| {},
        );
    });
    assert_eq!(launches.get(), 1, "one launch, then retired");
    assert!(
        logged
            .iter()
            .any(|line| line == "fabric bridge started: true (1 args)"),
        "the started line names its program: {} lines",
        logged.len()
    );
    assert!(
        !logged.iter().any(|line| line.contains("QZP7E7E")),
        "the log keeps the bridge's arguments"
    );
    assert_eq!(program_word("/opt/QZP7E7E2 x/bridge"), "bridge");
    assert_eq!(
        program_word("/opt/x/QZP7E7E3 bridge"),
        "?",
        "a name that is no word"
    );
}

#[test]
fn the_pre_flight_resolves_argv0_the_way_spawn_will() {
    use std::ffi::OsStr;
    let truth = true_program();
    let bin_dir = std::path::Path::new(&truth).parent().unwrap();

    assert_eq!(preflight(&truth, None), Ok(()));
    assert_eq!(preflight("/nonexistent/aterm-link", None), Err("not-found"));
    assert_eq!(
        preflight(bin_dir.to_str().unwrap(), None),
        Err("not-a-file")
    );

    assert_eq!(
        preflight("true", Some(OsStr::new("/nonexistent:/usr/bin:/bin"))),
        Ok(())
    );
    assert_eq!(
        preflight("true", Some(OsStr::new("/nonexistent"))),
        Err("not-on-path")
    );
    assert_eq!(preflight("true", None), Ok(()));

    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-fabric-path-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let dir = scratch.path();
    std::fs::write(dir.join("true"), b"not a program\n").unwrap();
    std::fs::set_permissions(
        dir.join("true"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
    )
    .unwrap();
    let mut path = dir.as_os_str().to_os_string();
    path.push(":");
    path.push(bin_dir.as_os_str());
    assert_eq!(
        preflight("true", Some(&path)),
        Ok(()),
        "a non-executable candidate is skipped"
    );
    assert_eq!(
        preflight("true", Some(dir.as_os_str())),
        Err("not-on-path"),
        "and alone it is no match at all"
    );
    assert_eq!(
        preflight(dir.join("true").to_str().unwrap(), None),
        Err("no-exec-bit"),
        "named by path, the same file is refused for its mode"
    );
}

#[cfg(target_vendor = "apple")]
#[test]
fn denied_metadata_does_not_decide_whether_a_bridge_may_execute() {
    const STAGE_ENV: &str = "ATERM_TEST_FABRIC_PREFLIGHT";
    const PROGRAM: &str = "/bin/echo";
    const RECEIPT: &str = "bridge-executed-without-metadata";
    if let Some(stage) = std::env::var_os(STAGE_ENV) {
        assert_eq!(
            std::fs::metadata(PROGRAM).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "the sandbox must deny the preflight's metadata read"
        );
        for program in [PROGRAM, "echo"] {
            assert_eq!(
                preflight(program, Some(std::ffi::OsStr::new("/bin"))),
                Ok(())
            );
            let result = aterm_uds::spawnfd::command(program)
                .arg(RECEIPT)
                .env("PATH", "/bin")
                .output();
            if stage == "execute" {
                let output = result.expect("the sandbox permits execution");
                assert!(output.status.success());
                assert_eq!(output.stdout, format!("{RECEIPT}\n").as_bytes());
            } else {
                assert_eq!(stage, "deny-execute");
                assert_eq!(
                    result
                        .expect_err("the sandbox must still refuse execution")
                        .kind(),
                    std::io::ErrorKind::PermissionDenied
                );
            }
        }
        println!("STAGED {stage:?} ok");
        return;
    }
    let name = format!(
        "{}::denied_metadata_does_not_decide_whether_a_bridge_may_execute",
        module_path!().split_once("::").expect("a crate's module").1
    );
    for (stage, extra) in [
        ("execute", ""),
        (
            "deny-execute",
            "(deny process-exec (literal \"/bin/echo\"))",
        ),
    ] {
        let policy = format!(
            "(version 1)(allow default)(deny file-read-metadata (literal \"/bin/echo\")){extra}"
        );
        let output = aterm_uds::deadline::ends_within(
            aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
                .args(["-p", &policy])
                .arg(std::env::current_exe().expect("the test binary"))
                .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                .env(STAGE_ENV, stage),
            aterm_uds::deadline::ECHO_HANG,
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(&format!("STAGED {stage:?} ok")),
            "{stage}: {:?}\nstdout: {stdout}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn link_moves_since(before: u64) -> Vec<(String, u64)> {
    crate::session_log_node::node_log()
        .lines()
        .into_iter()
        .filter_map(|line| {
            let seq = line.split_whitespace().next()?.parse::<u64>().ok()?;
            if seq <= before || !line.contains(" mark n link ") {
                return None;
            }
            let field = |prefix: &str| {
                line.split_whitespace()
                    .find_map(|word| word.strip_prefix(prefix))
            };
            Some((field("state=")?.to_string(), field("gen=")?.parse().ok()?))
        })
        .collect()
}

fn original_bridge_was_reaped(pid: i32, birth: aterm_uds::process::Birth) -> bool {
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(1),
        || {
            (aterm_uds::process::process_presence(pid)
                == aterm_uds::process::ProcessPresence::Absent
                || aterm_uds::process::read_birth(pid).is_some_and(|now| now != birth))
            .then_some(())
        },
    )
    .is_some()
}

#[test]
fn a_bridge_needs_both_lanes_and_failed_attachment_retires_its_generation() {
    use std::cell::{Cell, RefCell};
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::sync::Arc;

    crate::session_log_node::install();
    for mask in [[false, false], [true, false], [false, true], [true, true]] {
        crate::fabric::with_link_reset(|| {
            let before = crate::session_log_node::node_log().last_seq();
            let lanes = RefCell::new(Vec::new());
            let ended = RefCell::new(Vec::new());
            let generation = Cell::new(None);
            let proof = RefCell::new(None);
            let losses = Arc::new(Mutex::new(Vec::new()));
            let lost = Arc::clone(&losses);
            let store = crate::session_store::new_store();
            let argv = ["/bin/sh", "-c", "echo $$ >&3; read reply <&3"]
                .map(str::to_string)
                .to_vec();
            let result = launch_serving(
                &argv,
                |mut near, incarnation, token| {
                    let index = lanes.borrow().len();
                    generation.set(Some(incarnation));
                    if near
                        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                        .is_ok()
                        && index == 0
                    {
                        let mut line = String::new();
                        let read = std::io::BufReader::new(&mut near).read_line(&mut line);
                        let pid = line.trim().parse::<i32>().ok();
                        *proof.borrow_mut() = Some((
                            read,
                            pid.and_then(|pid| {
                                aterm_uds::process::read_birth(pid).map(|birth| (pid, birth))
                            }),
                        ));
                    }
                    lanes.borrow_mut().push(near);
                    if mask[index] {
                        ended.borrow_mut().push(token);
                    }
                    mask[index]
                },
                Told {
                    attached: crate::fabric::bridge_attached,
                    lost: Box::new(move |incarnation| {
                        lost.lock().unwrap().push(incarnation);
                        crate::fabric::bridge_lost(&store, incarnation);
                    }),
                },
            );
            let rejected = result.is_err();
            // Release an unexpectedly accepted partial launch too, so the regression fails
            // after cleaning up instead of hanging its supervisor behind this bridge.
            if let Ok(mut launched) = result {
                let _ = (&lanes.borrow()[0]).write_all(b"finish\n");
                let status = aterm_spec::wait::within(
                    aterm_spec::wait::MUST_HAPPEN,
                    Duration::from_millis(1),
                    || match launched.child.try_wait() {
                        Ok(Some(status)) => Some(Ok(status)),
                        Ok(None) => None,
                        Err(error) => Some(Err(error)),
                    },
                );
                launched.retire().watch_behind(LANES_END_WAIT);
                assert!(
                    status
                        .as_ref()
                        .is_some_and(|status| status.as_ref().is_ok_and(|status| status.success())),
                    "{mask:?}: {status:?}"
                );
            }
            let (read, identity) = proof
                .borrow_mut()
                .take()
                .expect("the verb callback read the execution proof");
            let (pid, birth) = identity.expect("the bridge wrote its live PID on fd 3");
            assert!(read.is_ok(), "{mask:?}: {read:?}");
            let reaped = original_bridge_was_reaped(pid, birth);
            let eof: Vec<_> = lanes
                .borrow_mut()
                .iter_mut()
                .map(|near| near.read(&mut [0u8; 1]).map_err(|error| error.kind()))
                .collect();
            let incarnation = generation.get().expect("both attachment callbacks ran");
            if mask.iter().any(|served| *served) {
                // A served lane may not reach its attach until the cleanup already told
                // the loss. Its late attach cannot resurrect the discarded generation.
                crate::fabric::bridge_attached(incarnation);
            }
            let moves = link_moves_since(before);
            let lost = losses.lock().unwrap().clone();
            ended.borrow_mut().clear();
            assert_eq!(rejected, mask != [true, true], "{mask:?}");
            assert!(reaped, "{mask:?}: the original bridge was not reaped");
            assert_eq!(
                eof,
                [Ok(0), Ok(0)],
                "{mask:?}: held lane endpoints are revoked"
            );
            if mask.iter().any(|served| *served) {
                assert_eq!(
                    lost,
                    [incarnation],
                    "{mask:?}: one synchronous generation loss"
                );
                assert_eq!(
                    moves
                        .iter()
                        .map(|(state, _)| state.as_str())
                        .collect::<Vec<_>>(),
                    ["stalled", "disconnected"],
                    "{mask:?}: {moves:?}"
                );
                assert_eq!(moves[0].1, moves[1].1, "{mask:?}: one generation");
            } else {
                assert!(
                    lost.is_empty() && moves.is_empty(),
                    "no served lane publishes an attach or loss: {lost:?} {moves:?}"
                );
            }
        });
    }
}

#[test]
fn a_failed_lane_attachment_backs_off_and_the_next_launch_can_serve_both() {
    use std::cell::{Cell, RefCell};
    use std::io::{BufRead as _, Write as _};
    let attempts = Cell::new(0usize);
    let first_failed = Cell::new(false);
    let kept = RefCell::new(Vec::new());
    let proof = RefCell::new(None);
    let pauses = RefCell::new(Vec::new());
    let argv = [true_program()];
    supervise_with(
        &argv,
        |argv| {
            let first = attempts.get() == 0;
            attempts.set(attempts.get() + 1);
            if !first {
                return launch_serving(argv, |_, _, _| true, untold());
            }
            let script = ["/bin/sh", "-c", "echo $$ >&3; read reply <&3"]
                .map(str::to_string)
                .to_vec();
            let result = launch_serving(
                &script,
                |mut near, _, _| {
                    let verb = kept.borrow().is_empty();
                    if verb
                        && near
                            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                            .is_ok()
                    {
                        let mut line = String::new();
                        if std::io::BufReader::new(&mut near)
                            .read_line(&mut line)
                            .is_ok()
                        {
                            *proof.borrow_mut() = line.trim().parse::<i32>().ok().and_then(|pid| {
                                aterm_uds::process::read_birth(pid).map(|birth| (pid, birth))
                            });
                        }
                    }
                    kept.borrow_mut().push(near);
                    verb
                },
                untold(),
            );
            first_failed.set(result.is_err());
            if result.is_ok() {
                let _ = (&kept.borrow()[0]).write_all(b"finish\n");
            }
            result
        },
        || attempts.get() == 2,
        |pause| pauses.borrow_mut().push(pause),
    );
    let (pid, birth) = proof
        .borrow_mut()
        .take()
        .expect("the failed bridge executed and wrote its PID");
    assert!(original_bridge_was_reaped(pid, birth));
    assert!(
        first_failed.get(),
        "one failed lane rejects the first launch"
    );
    assert_eq!(
        attempts.get(),
        2,
        "the next attempt serves both lanes and returns normally"
    );
    let pauses = pauses.into_inner();
    assert_eq!(pauses.len(), 2);
    assert_eq!(
        pauses[0],
        RELAUNCH_MIN * 2,
        "an attachment failure takes normal launch back-off"
    );
}

#[test]
fn the_lanes_are_retired_when_the_bridge_exits_whoever_else_holds_a_far_end() {
    use std::io::{BufRead as _, Read as _};
    use std::os::unix::net::UnixStream;
    let served = std::sync::Arc::new(std::sync::Mutex::new(Vec::<UnixStream>::new()));
    let recorder = {
        let served = std::sync::Arc::clone(&served);
        move |near: UnixStream, _generation, _ended| {
            served.lock().unwrap().push(near);
            true
        }
    };
    let argv: Vec<String> = ["/bin/sh", "-c", "sleep 300 & echo $! >&4; exit 0"]
        .iter()
        .map(ToString::to_string)
        .collect();
    let mut launched =
        launch_serving(&argv, recorder, untold()).expect("launch the stand-in bridge");
    let (mut verb, push) = {
        let mut near = served.lock().unwrap();
        assert_eq!(near.len(), 2, "both lanes served");
        let push = near.pop().unwrap();
        (near.pop().unwrap(), push)
    };
    push.set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    verb.set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(&push)
        .read_line(&mut line)
        .expect("the holder's pid");
    let holder: i32 = line.trim().parse().expect("a pid");
    let status = launched.wait().expect("reap the bridge");
    launched.retire();
    let mut byte = [0u8; 1];
    let read = verb.read(&mut byte);
    // SAFETY: sends a signal to the live holder pid started by this test's bridge.
    unsafe {
        libc::kill(holder, libc::SIGKILL);
    }
    assert!(status.success(), "{status:?}");
    assert_eq!(
        read.map_err(|e| e.kind()),
        Ok(0),
        "the near end read no end-of-file while another process held the far one"
    );
}

#[test]
fn a_lane_the_bridge_half_closed_is_retired_whoever_else_holds_a_far_end() {
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::net::UnixStream;
    let perl = ["/usr/bin/perl", "/bin/perl"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("a `perl` binary");
    let served = std::sync::Arc::new(std::sync::Mutex::new(Vec::<UnixStream>::new()));
    let recorder = {
        let served = std::sync::Arc::clone(&served);
        move |near: UnixStream, _generation, _ended| {
            served.lock().unwrap().push(near);
            true
        }
    };
    let script = format!(
        "{perl} -e 'open(my $s, \"+<&=\", 3) or die \"fd 3: $!\"; \
         shutdown($s, 1) or die \"shutdown: $!\"' || exit 1; \
         sleep 300 & echo $! >&4; exit 0"
    );
    let argv: Vec<String> = ["/bin/sh", "-c", &script]
        .iter()
        .map(ToString::to_string)
        .collect();
    let mut launched =
        launch_serving(&argv, recorder, untold()).expect("launch the stand-in bridge");
    let (verb, push) = {
        let mut near = served.lock().unwrap();
        assert_eq!(near.len(), 2, "both lanes served");
        let push = near.pop().unwrap();
        (near.pop().unwrap(), push)
    };
    push.set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(&push)
        .read_line(&mut line)
        .expect("the holder's pid");
    let holder: i32 = line.trim().parse().expect("a pid");
    let status = launched.wait().expect("reap the bridge");
    launched.retire();
    let wrote = (&verb).write(b"after the retire\n").map_err(|e| e.kind());
    // SAFETY: sends a signal to the live holder pid started by this test's bridge.
    unsafe {
        libc::kill(holder, libc::SIGKILL);
    }
    assert!(status.success(), "{status:?}");
    assert_eq!(
        wrote,
        Err(std::io::ErrorKind::BrokenPipe),
        "a write to a retired lane reached the far end's holder"
    );
}

#[test]
fn the_fabric_is_off_unless_it_is_configured() {
    if !crate::test_alone::alone() {
        return;
    }
    crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
        assert_eq!(configured_command(&Config::default()), None);
        assert_eq!(configured_command(&with_command(None)), None);
        assert_eq!(
            configured_command(&with_command(Some("   "))),
            None,
            "a blank command is OFF, not an empty argv"
        );
    });
}

#[test]
fn the_argv_form_keeps_a_spaced_program_whole_and_the_line_still_splits() {
    if !crate::test_alone::alone() {
        return;
    }
    crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
        let program = "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm";
        let argv = parsed(&format!(
            "[fabric]\ncommand = [\"{program}\", \"link\", \"serve\", \";\", \"it's\"]\n"
        ))
        .expect("an argv parses");
        assert_eq!(
            configured_command(&argv),
            Some(vec![
                program.to_string(),
                "link".to_string(),
                "serve".to_string(),
                ";".to_string(),
                "it's".to_string(),
            ])
        );
        let line = parsed(&format!("[fabric]\ncommand = \"{program} link serve\"\n"))
            .expect("a line parses");
        assert_eq!(
            configured_command(&line),
            Some(vec![
                "/Users//u/Applications/aterm".to_string(),
                "(dev).app/Contents/MacOS/aterm".to_string(),
                "link".to_string(),
                "serve".to_string(),
            ]),
            "the line form splits on whitespace, as it always has"
        );
        for off in ["command = []", "command = \"  \""] {
            let cfg = parsed(&format!("[fabric]\n{off}\n")).expect(off);
            assert_eq!(configured_command(&cfg), None, "{off} is the fabric off");
        }
    });
}

#[test]
fn a_malformed_argv_is_refused_with_its_reason() {
    for (value, why) in [
        (r#"["aterm", "", "serve"]"#, "element 2 is empty"),
        (r#"["aterm", "a\u0000b"]"#, "element 2 holds a NUL"),
        (r#"["aterm", 3]"#, "a string"),
        (r#"["aterm", ["link"]]"#, "a string"),
        ("3", "an array of strings"),
        ("true", "an array of strings"),
    ] {
        let err = parsed(&format!("[fabric]\ncommand = {value}\n"))
            .expect_err(&format!("{value} is refused"));
        assert!(err.contains(why), "{value}: {err}");
    }
    let fixed = parsed("[fabric]\ncommand = [\"aterm\", \"link\", \"serve\"]\n")
        .expect("the fixed array parses");
    assert_eq!(
        fixed.fabric.and_then(|f| f.command),
        Some(FabricCommand(vec![
            "aterm".to_string(),
            "link".to_string(),
            "serve".to_string()
        ]))
    );
}

#[test]
fn a_metacharacter_is_an_argument_and_not_a_second_command() {
    if !crate::test_alone::alone() {
        return;
    }
    let cfg = Config::default();
    crate::test_env::scoped(FABRIC_COMMAND_ENV, "bridge ; rm -rf /", || {
        assert_eq!(
            configured_command(&cfg),
            Some(vec![
                "bridge".to_string(),
                ";".to_string(),
                "rm".to_string(),
                "-rf".to_string(),
                "/".to_string(),
            ]),
            "a shell would have run the second command"
        );
    });
}
#[test]
fn a_healthy_run_relaunches_at_once_and_a_young_death_escalates_from_the_floor() {
    let instant = Duration::from_millis(5);

    assert_eq!(
        next_backoff(RELAUNCH_MAX, Some(RELAUNCH_HEALTHY)),
        Duration::ZERO
    );
    assert_eq!(
        next_backoff(RELAUNCH_MAX, Some(Duration::from_secs(3600))),
        Duration::ZERO
    );
    assert_eq!(
        next_backoff(
            RELAUNCH_MIN,
            Some(RELAUNCH_HEALTHY - Duration::from_millis(1))
        ),
        RELAUNCH_MIN * 2,
        "just short of healthy is a young death"
    );

    assert_eq!(
        next_backoff(Duration::ZERO, Some(instant)),
        RELAUNCH_MIN,
        "the relaunch that dies young waits again, or a healthy run could start a fork bomb"
    );
    assert_eq!(
        next_backoff(next_backoff(RELAUNCH_MAX, Some(RELAUNCH_HEALTHY)), None),
        RELAUNCH_MIN,
        "so does the relaunch that cannot start"
    );
    assert_eq!(next_backoff(RELAUNCH_MIN, None), RELAUNCH_MIN * 2);

    let mut backoff = RELAUNCH_MIN;
    for _ in 0..3 {
        backoff = next_backoff(backoff, Some(instant));
    }
    assert_eq!(backoff, RELAUNCH_MIN * 8, "an immediate exit must escalate");

    for _ in 0..20 {
        backoff = next_backoff(backoff, Some(instant));
    }
    assert_eq!(backoff, RELAUNCH_MAX);
}

/// What the loop itself waits, not only what [`next_backoff`] answers: nothing between a
/// healthy run and the next launch, and the floor, doubling, after each that dies young.
#[test]
fn the_loop_waits_nothing_after_a_healthy_run_and_the_floor_after_a_young_death() {
    let healthy = vec![
        ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("a `sleep` binary")
            .to_string(),
        (RELAUNCH_HEALTHY + Duration::from_millis(100))
            .as_secs_f64()
            .to_string(),
    ];
    let young = vec![true_program()];
    let launches = std::cell::Cell::new(0_u32);
    let paused = std::cell::RefCell::new(Vec::new());
    supervise_with(
        &young,
        |argv| {
            let first = launches.get() == 0;
            launches.set(launches.get() + 1);
            launch_serving(
                if first { &healthy } else { argv },
                |_, _, _| true,
                untold(),
            )
        },
        || launches.get() == 3,
        |waited| paused.borrow_mut().push(waited),
    );
    assert_eq!(
        paused.into_inner(),
        [Duration::ZERO, RELAUNCH_MIN, RELAUNCH_MIN * 2]
    );
}

/// What [`the_link_over_a_relaunch`] read.
struct Relaunch {
    /// What the link's hook told its hearer, the node log (`session_log_node::install`), as
    /// `(state, generation)` in order.
    heard: Vec<(String, u64)>,
    /// What the loop asked to wait after each bridge's exit.
    paused: Vec<Duration>,
    /// Whether both of the first bridge's lanes, hung up, were still up when the second
    /// bridge had attached and freed the session.
    freed_beside_the_first: bool,
    /// The governed session's hold once the first bridge's lanes had said their loss, the
    /// second bridge still up and serving it.
    hold_after_the_late_loss: Option<String>,
}

/// Two bridges under the supervisor's own loop, its pauses recorded and not slept, over one
/// session they govern, with `[fabric] hold_on_bridge_lost` on. Each bridge reads its verb
/// lane until aterm's end of it closes for writing, so its verb lane says when it exits.
///
/// The lanes are what `control::attach_fabric_bridge` and its `serve_bridge` do of a lane,
/// reduced to the attach, reading to the hang-up and then saying the loss (`BridgeLostGuard`).
/// The first bridge frees the session (a `hold off`, which governs it) and exits. Its lanes
/// stay past their hang-up until the second bridge has attached and freed the session as a
/// reconnecting bridge's `hold off` does. The second bridge stays up until both of them have
/// said their loss and the session's hold is read.
///
/// `exit_tells_loss` is the supervisor telling each generation's loss at its exit, as
/// `control::fabric_bridge_retired` does; with it off, the lanes alone say it.
fn the_link_over_a_relaunch(exit_tells_loss: bool) -> Relaunch {
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    crate::fabric::set_hold_on_bridge_lost(true);
    let log = crate::session_log_node::node_log();
    let before = log.last_seq();
    let store = crate::session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    let sid = handle.sid.as_str().to_string();
    let ctx = handle.ctx.clone();
    store.write().unwrap().register(handle);
    let argv: Vec<String> = ["/bin/sh", "-c", "exec cat <&3 >/dev/null"]
        .iter()
        .map(ToString::to_string)
        .collect();
    let threads = Arc::new(Mutex::new(Vec::new()));
    let freed = Arc::new(AtomicBool::new(false));
    let lingered = Arc::new(AtomicUsize::new(0));
    let late_said = Arc::new(AtomicUsize::new(0));
    let hold_read = Arc::new(Mutex::new(None::<Option<String>>));
    let answers = Arc::new(Mutex::new(Vec::new()));
    let exits = Arc::new(Mutex::new(Vec::new()));
    let lane = |first: bool| {
        let (store, sid, threads) = (store.clone(), sid.clone(), Arc::clone(&threads));
        let (freed, lingered) = (Arc::clone(&freed), Arc::clone(&lingered));
        let (late_said, hold_read, ctx) =
            (Arc::clone(&late_said), Arc::clone(&hold_read), ctx.clone());
        let (answers, exits) = (Arc::clone(&answers), Arc::clone(&exits));
        let lanes = AtomicUsize::new(0);
        move |near: UnixStream, generation: crate::fabric::BridgeGeneration, ended: LaneEnded| {
            let verb = lanes.fetch_add(1, Ordering::SeqCst) == 0;
            let (store, sid) = (store.clone(), sid.clone());
            let (freed, lingered) = (Arc::clone(&freed), Arc::clone(&lingered));
            let (late_said, hold_read, ctx) =
                (Arc::clone(&late_said), Arc::clone(&hold_read), ctx.clone());
            let answers = Arc::clone(&answers);
            let free = {
                let store = store.clone();
                move || {
                    let rest = format!("{sid} off");
                    crate::fabric::cmd_hold(&store, &rest, crate::fabric::HoldIssuer::Bridge)
                }
            };
            let must = |done: &dyn Fn() -> bool| {
                aterm_spec::wait::within(
                    aterm_spec::wait::MUST_HAPPEN,
                    Duration::from_millis(1),
                    || done().then_some(()),
                )
                .is_some()
            };
            // Asserting nothing here: a lane that panicked before its shutdown would leave its
            // bridge reading for good and the supervisor's wait with it. The test asserts what
            // the lanes recorded once every one has been joined.
            let exit = {
                let exits = Arc::clone(&exits);
                move |near: &UnixStream| {
                    let shut = near.shutdown(std::net::Shutdown::Write);
                    exits.lock().unwrap().push(shut.map_err(|e| e.kind()));
                }
            };
            let thread = crate::qos::spawn_lane_thread(
                "aterm-fabric-bridge".to_string(),
                crate::qos::Role::Responsive,
                move || {
                    let _ended = ended;
                    crate::fabric::set_lane_generation(generation);
                    crate::fabric::bridge_attached(generation);
                    if first {
                        if verb {
                            let answer = free();
                            exit(&near);
                            answers.lock().unwrap().push(("first", answer));
                        }
                        let _ = std::io::copy(&mut &near, &mut std::io::sink());
                        if must(&|| freed.load(Ordering::SeqCst)) {
                            lingered.fetch_add(1, Ordering::SeqCst);
                        }
                        crate::fabric::bridge_lost(&store, generation);
                        late_said.fetch_add(1, Ordering::SeqCst);
                    } else {
                        if verb {
                            let answer = free();
                            freed.store(true, Ordering::SeqCst);
                            let _ = must(&|| late_said.load(Ordering::SeqCst) == 2);
                            *hold_read.lock().unwrap() =
                                Some(ctx.fabric.hold().map(|hold| hold.reason));
                            exit(&near);
                            answers.lock().unwrap().push(("second", answer));
                        }
                        let _ = std::io::copy(&mut &near, &mut std::io::sink());
                        crate::fabric::bridge_lost(&store, generation);
                    }
                },
            )
            .expect("a lane's thread");
            threads.lock().unwrap().push(thread);
            true
        }
    };
    let told = || Told {
        attached: crate::fabric::bridge_attached,
        lost: if exit_tells_loss {
            let store = store.clone();
            Box::new(move |generation| {
                crate::fabric::bridge_lost(&store, generation);
            })
        } else {
            Box::new(|_| {})
        },
    };
    let launches = std::cell::Cell::new(0_u32);
    let paused = std::cell::RefCell::new(Vec::new());
    supervise_with(
        &argv,
        |argv| {
            let first = launches.get() == 0;
            launches.set(launches.get() + 1);
            launch_serving(argv, lane(first), told())
        },
        || launches.get() == 2,
        |waited| paused.borrow_mut().push(waited),
    );
    for thread in std::mem::take(&mut *threads.lock().unwrap()) {
        thread.join().expect("a lane's thread");
    }
    assert_eq!(
        *exits.lock().unwrap(),
        [Ok(()), Ok(())],
        "each bridge's verb lane shut for writing"
    );
    assert_eq!(
        *answers.lock().unwrap(),
        [
            ("first", "OK hold=0\n".to_string()),
            ("second", "OK hold=0\n".to_string())
        ],
        "the first bridge governs the session, and the second frees it"
    );
    let word = |line: &str, key: &str| -> String {
        line.split(key)
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .unwrap_or_else(|| panic!("no{key} in {line}"))
            .to_string()
    };
    let heard = log
        .lines()
        .into_iter()
        .filter(|l| {
            l.split(' ').next().and_then(|s| s.parse::<u64>().ok()) > Some(before)
                && l.contains(" mark n link ")
        })
        .map(|l| {
            let generation = word(&l, " gen=").parse().expect("a generation");
            (word(&l, " state="), generation)
        })
        .collect();
    let hold_after_the_late_loss = hold_read
        .lock()
        .unwrap()
        .take()
        .expect("the second bridge read the hold");
    Relaunch {
        heard,
        paused: paused.into_inner(),
        freed_beside_the_first: lingered.load(Ordering::SeqCst) == 2,
        hold_after_the_late_loss,
    }
}

/// THE ORDER A RELAUNCH AT ONCE RESTS ON, however long the old generation's lanes take: the
/// first bridge's lanes say their loss only after the second has attached, and the link still
/// reads the first `disconnected` ahead of the second's attach. Its negative control: the same
/// relaunch with the loss said by the lanes alone (planted by `exit_tells_loss` off) loses the
/// first one's `Lost` to the second one's attach.
#[test]
fn a_relaunched_generation_attaches_after_the_old_generations_loss() {
    crate::session_log_node::install();
    crate::fabric::with_link_reset(|| {
        let Relaunch { heard, .. } = the_link_over_a_relaunch(true);
        let states: Vec<&str> = heard.iter().map(|(state, _)| state.as_str()).collect();
        assert_eq!(
            states,
            ["stalled", "disconnected", "stalled", "disconnected"],
            "attach, loss, attach, loss: {heard:?}"
        );
        assert_eq!(heard[0].1, heard[1].1, "{heard:?}");
        assert_eq!(heard[2].1, heard[3].1, "{heard:?}");
        assert!(heard[1].1 < heard[2].1, "{heard:?}");
    });
    crate::fabric::with_link_reset(|| {
        let Relaunch { heard, .. } = the_link_over_a_relaunch(false);
        let lost: Vec<u64> = heard
            .iter()
            .filter(|(state, _)| state == "disconnected")
            .map(|(_, generation)| *generation)
            .collect();
        let attached = heard.iter().rev().find(|(state, _)| state == "stalled");
        assert_eq!(
            Some(lost.as_slice()),
            attached.map(|(_, generation)| std::slice::from_ref(generation)),
            "said by the lanes alone, the old generation's loss lands past the attach and is \
             no transition: {heard:?}"
        );
    });
}

/// A RELAUNCH WAITS ON NO LANE OF THE BRIDGE IT REPLACES, AND THAT BRIDGE'S LATE LOSS HOLDS NO
/// SESSION THE NEW ONE FREED. Under `[fabric] hold_on_bridge_lost`, the first bridge's exit
/// holds the session it governed; the second launches after the back-off alone, while the
/// first's lanes are still up, and frees it. The first's lanes then say their loss, a dead
/// generation's, and the session stays free.
///
/// NEGATIVE CONTROL: without the `superseded` check on `HoldAct::Lost` in
/// `aterm_gui_fabric::apply_hold_as`, the late loss holds the session again
/// (`hold_after_the_late_loss` reads `Some("fabric-lost")`, 2026-11-05).
#[test]
fn a_relaunch_waits_on_no_old_lane_and_the_old_ones_late_loss_holds_no_session() {
    crate::session_log_node::install();
    crate::fabric::with_link_reset(|| {
        let relaunch = the_link_over_a_relaunch(true);
        assert_eq!(
            relaunch.paused,
            [RELAUNCH_MIN * 2, RELAUNCH_MIN * 4],
            "the loop waits the back-off after each young death and nothing else"
        );
        assert!(
            relaunch.freed_beside_the_first,
            "the second bridge attached and freed the session while the first's lanes were \
             still up: {:?}",
            relaunch.heard
        );
        assert_eq!(
            relaunch.hold_after_the_late_loss, None,
            "a dead generation's late loss held a session the new bridge serves"
        );
    });
}

#[test]
fn lanes_that_do_not_end_are_warned_of_and_hold_no_relaunch_up() {
    use std::os::unix::net::UnixStream;
    let kept = std::sync::Arc::new(Mutex::new(Vec::<(UnixStream, LaneEnded)>::new()));
    let recorder = {
        let kept = std::sync::Arc::clone(&kept);
        move |near: UnixStream, _generation: crate::fabric::BridgeGeneration, ended: LaneEnded| {
            kept.lock().unwrap().push((near, ended));
            true
        }
    };
    let argv = vec![true_program()];
    let warning = |logged: &[String]| logged.iter().any(|l| l.contains("had not ended"));

    let mut wedged =
        launch_serving(&argv, &recorder, untold()).expect("launch the stand-in bridge");
    wedged.wait().expect("reap the bridge");
    let retiring = wedged.retire();
    let logged = crate::test_log::logged_while(|| {
        assert!(!retiring.watch(Duration::from_millis(1)), "a lane is held");
    });
    assert!(warning(&logged), "{logged:?}");

    let mut ended =
        launch_serving(&argv, |_, _, _| true, untold()).expect("launch the stand-in bridge");
    ended.wait().expect("reap the bridge");
    let retiring = ended.retire();
    let logged = crate::test_log::logged_while(|| {
        assert!(retiring.watch(LANES_END_WAIT), "no lane is held");
    });
    assert!(!warning(&logged), "{logged:?}");
}

#[test]
fn the_bridge_child_inherits_no_identity_no_socket_and_no_edge_secret() {
    let pair = |k: &str, v: &str| (std::ffi::OsString::from(k), std::ffi::OsString::from(v));
    let inherited = vec![
        pair("PATH", "/usr/bin"),
        pair("HOME", "/home/a"),
        pair("XDG_STATE_HOME", "/home/a/.local/state"),
        pair("ATERM_LINK_FAULT", "kill-after-publish"),
        pair("ATERM_LINK_WAIT_TALLY", "1"),
        pair("ATERM_EDGE_READ", "0011"),
        pair("ATERM_EDGE_WRITE", "2233"),
        pair("ATERM_EDGE_SIGNAL", "4455"),
        pair("ATERM_EDGE_TOKENS", "/run/aterm/edge.tok"),
        pair("ATERM_SESSION_ID", "s-0123456789abcdef0123"),
        pair("ATERM_LAUNCH_NONCE", "0".repeat(32).as_str()),
        pair("ATERM_LINK_BROKER", "/run/aterm/broker.sock"),
        pair("ATERM_LINK_CAP_FILE", "/etc/aterm/outer.cap"),
        pair("ATERM_LINK_FLEET", "outer"),
        pair("ATERM_FABRIC_COMMAND", "aterm-link serve --fleet outer"),
        pair("ANTHROPIC_API_KEY", "sk-x"),
        pair("ATERM_DEV_FAST_TIMERS", "1"),
    ];
    let kept = filter_child_env(inherited.into_iter());
    let has = |k: &str| kept.iter().any(|(n, _)| n == std::ffi::OsStr::new(k));

    for secret in [
        "ATERM_EDGE_READ",
        "ATERM_EDGE_WRITE",
        "ATERM_EDGE_SIGNAL",
        "ATERM_EDGE_TOKENS",
        "ATERM_SESSION_ID",
        "ATERM_LAUNCH_NONCE",
        "ATERM_LINK_BROKER",
        "ATERM_LINK_CAP_FILE",
        "ATERM_LINK_FLEET",
        "ATERM_FABRIC_COMMAND",
        "ANTHROPIC_API_KEY",
    ] {
        assert!(!has(secret), "the bridge child must not inherit {secret}");
    }

    for keeper in [
        "PATH",
        "HOME",
        "XDG_STATE_HOME",
        "ATERM_LINK_FAULT",
        "ATERM_LINK_WAIT_TALLY",
    ] {
        assert!(has(keeper), "the bridge child still needs {keeper}");
    }
    assert_eq!(
        has("ATERM_DEV_FAST_TIMERS"),
        cfg!(any(debug_assertions, feature = "dev-seams")),
        "the bridge runs its instance's clock"
    );
}

#[test]
fn the_supervisor_arms_once_and_records_what_it_runs() {
    crate::fabric::with_link_reset(|| {
        let fresh = status();
        assert!(fresh.armed.is_none() && fresh.configured.is_none());
        assert!(!crate::fabric::bridge_supervised());
        assert!(matches!(arm(Vec::new()), Err(ArmError::NoCommand)));
        assert!(
            !crate::fabric::bridge_supervised(),
            "an empty argv must not flip the endpoint's latch"
        );

        for (argv0, reason) in [
            ("/nonexistent/aterm-link", "not-found"),
            ("/", "not-a-file"),
            ("aterm-link-no-such-program-4f2c", "not-on-path"),
        ] {
            match arm(vec![argv0.to_string(), "serve".to_string()]) {
                Err(ArmError::NotExecutable {
                    program,
                    reason: got,
                }) => {
                    assert_eq!(program, argv0);
                    assert_eq!(got, reason, "{argv0}");
                }
                other => panic!("{argv0} must be refused as not executable: {other:?}"),
            }
            assert!(status().armed.is_none(), "{argv0} must arm nothing");
            assert!(
                !crate::fabric::bridge_supervised(),
                "{argv0} must not flip the endpoint's latch"
            );
        }
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-fabric-preflight-")
            .suffix(".noindex")
            .tempdir()
            .unwrap();
        let plain = scratch.path().join("not-a-program");
        std::fs::write(&plain, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            &plain,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
        )
        .unwrap();
        let plain_str = plain.to_string_lossy().into_owned();
        assert!(matches!(
            arm(vec![plain_str.clone()]),
            Err(ArmError::NotExecutable {
                reason: "no-exec-bit",
                ..
            })
        ));
        assert!(status().armed.is_none() && !crate::fabric::bridge_supervised());

        let argv = vec![true_program(), "serve".to_string()];
        assert!(
            arm(argv.clone()).is_ok(),
            "the refused attempts left the latch open"
        );
        assert!(crate::fabric::bridge_supervised(), "arming IS the latch");
        assert_eq!(status().armed.as_deref(), Some(argv.as_slice()));

        let second = vec!["/nonexistent/other".to_string()];
        match arm(second) {
            Err(ArmError::AlreadySupervised(running)) => assert_eq!(running, argv),
            other => panic!("a second arm must be refused: {other:?}"),
        }
        assert_eq!(
            status().armed.as_deref(),
            Some(argv.as_slice()),
            "the first argv stays"
        );
    });
}

#[test]
fn the_startup_path_records_the_configured_command_and_arms_through_the_one_seam() {
    if !crate::test_alone::alone() {
        return;
    }
    crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
        crate::fabric::with_link_reset(|| {
            assert!(!spawn_supervisor(&Config::default()));
            let s = status();
            assert!(s.armed.is_none() && s.configured.is_none());
            assert!(!crate::fabric::bridge_supervised());
        });
        crate::fabric::with_link_reset(|| {
            assert!(!spawn_supervisor(&with_command(Some(
                "/nonexistent/aterm-link serve --fleet lab"
            ))));
            let s = status();
            assert_eq!(
                s.configured,
                Some(vec![
                    "/nonexistent/aterm-link".to_string(),
                    "serve".to_string(),
                    "--fleet".to_string(),
                    "lab".to_string(),
                ])
            );
            assert!(s.armed.is_none(), "a command that cannot run arms nothing");
            assert!(!crate::fabric::bridge_supervised());
        });
        crate::fabric::with_link_reset(|| {
            let program = true_program();
            assert!(spawn_supervisor(&with_command(Some(&format!(
                "{program} serve --fleet lab"
            )))));
            let want = vec![
                program,
                "serve".to_string(),
                "--fleet".to_string(),
                "lab".to_string(),
            ];
            let s = status();
            assert_eq!(s.configured, Some(want.clone()));
            assert_eq!(s.armed, Some(want));
            assert!(crate::fabric::bridge_supervised());
        });
    });
}

#[test]
fn a_runtime_attached_bridge_is_armed_again_after_an_update() {
    if !crate::test_alone::alone() {
        return;
    }
    use crate::session_store::{SessionHandoff, SessionStore};
    crate::test_env::scoped_unset(FABRIC_COMMAND_ENV, || {
        let argv = vec![true_program(), "serve".to_string(), "--fleet".to_string()];
        let wire = crate::fabric::with_link_reset(|| {
            assert!(!spawn_supervisor(&Config::default()));
            arm(argv.clone()).expect("the attach arms");
            assert_eq!(handoff_attached(), argv);
            let manifest = SessionHandoff::from_store(&SessionStore::default());
            assert_eq!(manifest.fabric_attached, argv);
            manifest.to_toml().expect("serializes")
        });
        crate::fabric::with_link_reset(|| {
            let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
            carry_attached(read.fabric_attached);
            assert!(spawn_supervisor(&Config::default()));
            let s = status();
            assert_eq!(s.armed, Some(argv.clone()), "the attached bridge is back");
            assert_eq!(s.configured, None);
            assert!(crate::fabric::bridge_supervised());
            assert_eq!(handoff_attached(), argv);
        });
        crate::fabric::with_link_reset(|| {
            let program = true_program();
            assert!(spawn_supervisor(&with_command(Some(&format!(
                "{program} serve"
            )))));
            assert!(handoff_attached().is_empty());
        });
        crate::fabric::with_link_reset(|| {
            carry_attached(vec!["/nonexistent/carried".to_string()]);
            let program = true_program();
            assert!(spawn_supervisor(&with_command(Some(&format!(
                "{program} serve"
            )))));
            assert_eq!(status().armed, Some(vec![program, "serve".to_string()]));
        });
    });
}

#[test]
fn a_quit_says_bye_to_every_watcher_and_waits_only_for_a_bridge_to_take_it() {
    crate::fabric::with_link_reset(|| {
        let store = crate::session_store::new_store();
        let seq = |store: &crate::session_store::Store| {
            store.read().unwrap_or_else(|p| p.into_inner()).roster_seq()
        };
        assert!(!bye(&store, Bye::Quit, aterm_spec::wait::MUST_HAPPEN));
        assert_eq!(seq(&store), 1, "no bridge, and said all the same");

        let generation = crate::fabric::next_bridge_generation();
        crate::fabric::bridge_attached(generation);
        assert!(!bye(&store, Bye::Handoff, Duration::ZERO));
        assert_eq!(seq(&store), 2, "said, and not waited for");

        let bridge = {
            let store = store.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                while seq(&store) < 3 {
                    assert!(Instant::now() < deadline, "the quit was never said");
                    aterm_uds::deadline::poll_yield();
                }
                crate::fabric::cmd_link(&store, "bye quit", Some(generation))
            })
        };
        assert!(bye(&store, Bye::Quit, aterm_spec::wait::MUST_HAPPEN));
        assert_eq!(bridge.join().expect("the bridge's answer"), "OK\n");
    });
}

#[test]
fn a_retired_supervisor_relaunches_nothing() {
    crate::fabric::with_link_reset(|| {
        RETIRED.store(true, Ordering::SeqCst);
        let (done, returned) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            supervise(&["/nonexistent/aterm-link".to_string()]);
            let _ = done.send(());
        });
        assert!(
            returned.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok(),
            "a retired supervisor went on launching"
        );
    });
}

/// The Overview reads the fabric where the supervisor names a command either way: armed
/// alone (a bridge `aterm fabric on` attached) or configured alone (a `[fabric] command`
/// whose preflight failed), as well as both; with neither it names none.
#[test]
fn an_armed_or_a_configured_command_alone_names_the_bridge() {
    let status = |armed: bool, configured: bool| SupervisorStatus {
        armed: armed.then(|| vec!["bridge".to_owned()]),
        configured: configured.then(|| vec!["bridge".to_owned()]),
    };
    assert!(!status(false, false).names_a_command());
    assert!(status(true, false).names_a_command(), "attached at runtime");
    assert!(status(false, true).names_a_command(), "configured, unarmed");
    assert!(status(true, true).names_a_command());
}

#[test]
fn a_quit_says_quit_unless_the_seam_names_handoff() {
    use std::ffi::OsStr;
    assert_eq!(quit_bye(None), Bye::Quit);
    assert_eq!(quit_bye(Some(OsStr::new("handoff"))), Bye::Handoff);
    for other in ["", "quit", "1", "Handoff"] {
        assert_eq!(quit_bye(Some(OsStr::new(other))), Bye::Quit, "{other:?}");
    }
}
