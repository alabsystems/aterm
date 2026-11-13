// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A TERM queued after the old image blocks the handoff set stays pending across a real
/// native exec, then reaches the new image's handler only after it is installed. HUP, which
/// was already blocked, stays blocked. NEGATIVE CONTROL: a corrupted private description is
/// refused and restores nothing, leaving the queued TERM blocked until that probe exits.
#[cfg(target_vendor = "apple")]
#[test]
fn a_queued_signal_survives_exec_and_a_malformed_handoff_restores_nothing() {
    use std::time::Duration;

    for refused in [true, false] {
        let mut actor = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("aterm-release"));
        actor.env(EXEC_PROBE_STAGE_ENV, "actor");
        if refused {
            actor.env(EXEC_PROBE_REFUSE_ENV, "1");
        }
        let output =
            aterm_uds::deadline::output_within(&mut actor, Duration::from_secs(60)).unwrap();
        assert!(
            output.status.success(),
            "signal exec actor failed ({:?}): stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let output = String::from_utf8_lossy(&output.stdout);
        let expected = if refused {
            "signal-exec-refused-malformed-handoff"
        } else {
            "signal-exec-caught-queued-term-after-handler-install"
        };
        assert!(output.contains(expected), "missing {expected}: {output}");
    }
}

/// A guard that closed before its worker was scheduled must not read stale signals,
/// wait, or dispatch to the completed step's former numeric process group.
#[cfg(unix)]
#[test]
fn an_already_closed_forwarder_never_looks_or_sends() {
    forward_with(&Forwarding {
        active: &|| false,
        caught: &|| panic!("a closed step's signal count was read"),
        signal: &|| panic!("a closed step's signal was read"),
        clock: &|| panic!("a closed step's clock was read"),
        send: &|_| panic!("a completed step's old group was signalled"),
        pause: &|| panic!("a closed step waited before checking cancellation"),
    });
}

/// Closing while a signal/clock observation is in progress fences either dispatch,
/// including escalation after a previously sent first stop. No native signal is sent.
#[cfg(unix)]
#[test]
fn cancellation_during_a_look_fences_first_and_escalated_sends() {
    use std::cell::{Cell, RefCell};
    use std::time::{Duration, Instant};
    for cancel_look in [1, 2] {
        let active = Cell::new(true);
        let look = Cell::new(1);
        let sends = RefCell::new(Vec::new());
        let t0 = Instant::now();
        forward_with(&Forwarding {
            active: &|| active.get(),
            caught: &|| {
                if look.get() == cancel_look {
                    active.set(false);
                }
                look.get()
            },
            signal: &|| SIGHUP,
            clock: &|| t0 + Duration::from_secs(u64::from(look.get())),
            send: &|sig| sends.borrow_mut().push(sig),
            pause: &|| {
                look.set(look.get() + 1);
                active.get()
            },
        });
        assert_eq!(
            *sends.borrow(),
            if cancel_look == 1 {
                vec![]
            } else {
                vec![SIGHUP]
            },
            "the cancelled look must not send its first stop or escalation"
        );
    }
}

/// One stop that reaches the release along several paths sends the step's group one signal;
/// a stop found [`ESCALATE_AFTER`] or more after the first send ended ([`Stop::sent`]) kills
/// the group, and so does each after it. The clock is the test's: each answer is asked at a
/// moment it names. NEGATIVE CONTROLS: kill on any signal found after the first, and the
/// burst of copies kills the group; take only the time from [`Stop::sent`], and a copy caught
/// while the first send ran, found the interval after it ended, kills the group.
#[cfg(unix)]
#[test]
fn copies_of_one_stop_count_once_and_a_later_stop_kills_the_group() {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut stop = Stop::default();
    assert_eq!(stop.next(0, at(0)), None, "nothing caught");
    assert_eq!(
        stop.next(2, at(50)),
        Some(Pass::First),
        "the first stop, and a copy"
    );
    stop.sent(3, at(400));
    assert_eq!(
        stop.next(5, at(450)),
        None,
        "a burst of copies found after it ended"
    );
    assert_eq!(
        stop.next(6, at(1399)),
        None,
        "a copy found just inside the interval after the send ended"
    );
    assert_eq!(stop.next(6, at(9000)), None, "nothing caught since");
    assert_eq!(
        stop.next(7, at(9000)),
        Some(Pass::Kill),
        "a person's second stop"
    );
    assert_eq!(
        stop.next(9, at(9050)),
        Some(Pass::Kill),
        "and each after it"
    );

    let mut stop = Stop::default();
    assert_eq!(stop.next(1, at(0)), Some(Pass::First));
    stop.sent(2, at(2000));
    let after = at(2000) + ESCALATE_AFTER;
    assert_eq!(
        stop.next(2, after),
        None,
        "a copy caught while the first send ran, found the interval after it ended"
    );
    assert_eq!(
        stop.next(3, after),
        Some(Pass::Kill),
        "a stop found the interval after the first send ended"
    );
}

/// The signals [`forward_with`] sends for a run of looks on the test's clock: the first send
/// takes `first_send_ms` and `copies_in_send` copies are caught while it runs; each pause
/// lasts its milliseconds and catches its count of signals. Each send is listed with the
/// milliseconds from the start at which it began.
#[cfg(unix)]
fn forwarded(first_send_ms: u64, copies_in_send: u32, pauses: &[(u64, u32)]) -> Vec<(i32, u64)> {
    use std::cell::{Cell, RefCell};
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let clock = Cell::new(t0);
    let caught = Cell::new(0u32);
    let sends = RefCell::new(Vec::new());
    let next = Cell::new(0usize);
    let since = |at: Instant| u64::try_from(at.duration_since(t0).as_millis()).unwrap();
    forward_with(&Forwarding {
        active: &|| true,
        caught: &|| caught.get(),
        signal: &|| SIGHUP,
        clock: &|| clock.get(),
        send: &|sig| {
            let first = sends.borrow().is_empty();
            sends.borrow_mut().push((sig, since(clock.get())));
            if first {
                caught.set(caught.get() + copies_in_send);
                clock.set(clock.get() + Duration::from_millis(first_send_ms));
            }
        },
        pause: &|| {
            let Some(&(ms, signals)) = pauses.get(next.get()) else {
                return false;
            };
            next.set(next.get() + 1);
            clock.set(clock.get() + Duration::from_millis(ms));
            caught.set(caught.get() + signals);
            true
        },
    });
    sends.into_inner()
}

/// The forwarding loop sends the signal that interrupted the release once for the first stop,
/// however long that send takes: a copy caught while it runs, or found within
/// [`ESCALATE_AFTER`] after it ended, sends nothing, and a stop found that long after it
/// ended sends `SIGKILL`. NEGATIVE CONTROLS: time the interval from the look that found the
/// first signal (drop [`Stop::sent`]), and the copy found 50 ms after a 2 s send kills the
/// group; take only the time from [`Stop::sent`], not the copies caught by then, and the copy
/// caught during the send but found by a thread that looked again 1.5 s after it kills the
/// group; send nothing for [`Pass::Kill`], and the person's second stop sends nothing; send
/// `SIGTERM` for the first stop, and the `SIGHUP` that interrupted the release is not sent.
#[cfg(unix)]
#[test]
fn a_copy_caught_during_a_slow_first_send_counts_once_and_a_later_stop_kills() {
    assert_eq!(
        forwarded(2000, 1, &[(50, 1), (50, 1), (1000, 1)]),
        [(SIGHUP, 50), (SIGKILL, 3100)],
        "the stop at 50 ms, a copy caught during its 2 s send, one found 50 ms after it, \
         and a second stop found 1050 ms after it"
    );
    assert_eq!(
        forwarded(2000, 1, &[(50, 1), (1500, 0)]),
        [(SIGHUP, 50)],
        "a copy caught during the send, found 1.5 s after it ended"
    );
    assert_eq!(
        forwarded(0, 0, &[(50, 1), (50, 1), (949, 1), (1, 1)]),
        [(SIGHUP, 50), (SIGKILL, 1050)],
        "a copy 50 ms and one 999 ms after an instant send, then a stop 1000 ms after it"
    );
}

/// The rows of a `pid ppid pgid name` table, one process a line; a line that does not read
/// is skipped.
fn rows(table: &str) -> Vec<Proc> {
    table
        .lines()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut num = || -> Option<i32> {
                let end = rest.find(char::is_whitespace)?;
                let n = rest[..end].parse().ok()?;
                rest = rest[end..].trim_start();
                Some(n)
            };
            let (pid, parent, pgid) = (num()?, num()?, num()?);
            Some(Proc {
                pid,
                parent,
                pgid,
                name: rest.trim_end().to_string(),
            })
        })
        .collect()
}

/// The release (400) started the step's group 500; its member 502 is a trustd, and 503 and
/// 504 its solvers.
#[test]
fn the_interrupt_spares_a_trustd_and_its_solvers() {
    let listing = rows(
        "\
        1     0     1 /sbin/launchd\n\
      500   400   500 /bin/sh\n\
      501   500   500 /Users//x/Library/Application Support/aterm/pkg/trust/bin/targo\n\
      502   501   500 /Users//x/Library/Application Support/aterm/pkg/trust/bin/trustd\n\
      503   502   500 z3\n\
      504   503   500 z3-worker\n\
      505   500   500 sleep\n\
      600   500   600 /usr/libexec/trustd\n\
      601     1   777 /bin/sh\n\
      garbage line\n",
    );
    assert_eq!(signalled(&listing, 500, 400), [500, 501, 505]);
    assert_eq!(signalled(&listing, 777, 400), [601]);
    assert!(signalled(&listing, 999, 400).is_empty());
}

/// Below the process that started the step (`home`, 10), a parent the listing lacks (20) or
/// a loop of reused pids (31, 32) spares a member, since what was not read could be a
/// trustd; from `home` up, one the listing lacks (7, above the shell 8) is no trustd, and a
/// trustd there spares every member `home` started, as `ps`'s listing did. NEGATIVE
/// CONTROLS: take a parent the listing lacks below `home` for no trustd, and 21 is
/// signalled; answer a loop's end with a signal, and 31 and 32 are; take one lacking above
/// `home` for a trustd, and only the orphan 13 is; end the climb at `home` unjudged, and the
/// trustd above it spares nothing.
#[test]
fn a_climb_spares_what_it_could_not_read_below_home_and_judges_homes_own_parents() {
    let table = |shell: &str| {
        rows(&format!(
            "\
             8     7     8 {shell}\n\
            10     8    10 aterm-release\n\
            11    10    11 targo\n\
            12    11    11 rustc\n\
            13     1    11 orphan\n\
            21    20    11 cc\n\
            31    32    11 a\n\
            32    31    11 b\n"
        ))
    };
    assert_eq!(
        signalled(&table("zsh"), 11, 10),
        [11, 12, 13],
        "what home started, and an orphan of it"
    );
    assert_eq!(
        signalled(&table("trustd"), 11, 10),
        [13],
        "under a trustd, only the orphan launchd took"
    );
    let mut under_one = table("zsh");
    under_one.push(Proc {
        pid: 1,
        parent: 0,
        pgid: 1,
        name: "trustd".to_string(),
    });
    assert_eq!(
        signalled(&under_one, 11, 10),
        [11, 12],
        "a trustd at pid 1 spares the orphan it took"
    );
}

/// `/proc/<pid>/stat` reads as the row of the pid it names, its name between the first `(`
/// and the last `)`. NEGATIVE CONTROL: drop the pid check, and the text of another pid reads
/// as this one's.
#[test]
fn a_proc_stat_line_reads_as_the_row_of_its_own_pid() {
    let stat = "4242 (a) b (trustd)) S 4200 4100 4100 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 \
                123 0 0 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
    assert_eq!(
        stat_proc(stat, 4242),
        Some(Proc {
            pid: 4242,
            parent: 4200,
            pgid: 4100,
            name: "a) b (trustd)".to_string(),
        })
    );
    assert_eq!(stat_proc(stat, 4243), None, "the text of another pid");
    assert_eq!(stat_proc("4242 (sh", 4242), None, "a cut line");
}

/// The lines `from` carries, read on a thread: each `Some`, then `None` at its end.
#[cfg(unix)]
fn lines(from: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<Option<String>> {
    use std::io::BufRead as _;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(from).lines() {
            let Ok(line) = line else { break };
            if tx.send(Some(line)).is_err() {
                return;
            }
        }
        let _ = tx.send(None);
    });
    rx
}

/// The next of `lines`, within the one-minute hang detector.
#[cfg(unix)]
#[track_caller]
fn next(lines: &std::sync::mpsc::Receiver<Option<String>>, what: &str) -> Option<String> {
    lines
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("{what}: nothing within the one-minute hang detector: {e}"))
}

/// `child`'s exit status, within the one-minute hang detector.
#[cfg(unix)]
#[track_caller]
fn ended(child: &mut aterm_uds::spawnfd::Child, what: &str) -> std::process::ExitStatus {
    let bound = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            std::time::Instant::now() < bound,
            "{what} did not end within the one-minute hang detector"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A real process group of this test's own: a leader shell, a grandchild shell it started,
/// and a stand-in named `trustd` (a copy of a shell, joined to the group) running `cat`, its
/// child. One SIGTERM sent to the group ends the leader by that signal (its wait status) and
/// the grandchild by its own handler (the line it prints), and their output pipe ends. The
/// stand-in's `cat` still echoes a line written after the send returned (a SIGTERM sent
/// before that write would have ended it first), then ends at its input's end with status 0,
/// and the stand-in exits 0. Each process here ends by itself once this test's pipes close.
/// NEGATIVE CONTROLS: send the leader alone, and the grandchild never prints `TERM`; judge a
/// member by its own name only, not climbing, and the trustd's `cat` ends by the signal
/// (`cat 143`) instead of echoing; take no member for a trustd, and nothing echoes either;
/// spare what descends from a trustd but not the trustd, and it dies by SIGTERM before it
/// prints `cat 0`.
#[cfg(unix)]
#[test]
fn a_real_group_is_signalled_but_for_a_trustd_and_what_it_runs() {
    use aterm_uds::spawnfd::{Stdio, command};
    use std::io::Write as _;
    use std::os::unix::process::ExitStatusExt as _;
    let mut kept =
        aterm_uds::deadline::KeptUntilRan::new(crate::test_scratch_dir("interrupt").keep());
    let trustd = kept.path().join("trustd");
    let shell = if cfg!(target_vendor = "apple") {
        "/bin/dash"
    } else {
        "/bin/sh"
    };
    aterm_uds::deadline::ad_hoc_copy(std::path::Path::new(shell), &trustd);
    // The copy shows it runs from its file (macOS has assessed its first exec) by a line it
    // writes, before anything may remove it; one macOS still holds at the hang detector
    // ends the test COULD NOT RUN.
    let mut proof = command(&trustd)
        .args(["-c", "echo ran"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    aterm_uds::deadline::prints(&mut proof, "ran", aterm_uds::deadline::ECHO_HANG);
    let _ = proof.wait();
    kept.ran();
    // The group starts with the signals that end a process at their defaults, set here, not
    // this test binary's: a shell started ignoring SIGTERM (a `nohup` or `&` runner's
    // choice, which every exec keeps) can trap none, and neither ends nor says `TERM`.
    let start = aterm_spec::start::at_defaults(&aterm_spec::start::TERMINATING);
    let mut leader = command("/bin/sh")
        .args([
            "-c",
            "/bin/sh -c 'trap \"echo TERM \\$\\$; exit 0\" TERM; echo up $$; \
             while kill -0 $PPID 2>/dev/null; do /bin/sleep 1; done' & read -r _",
        ])
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(start);
    let group = i32::try_from(leader.id()).unwrap();
    let from_group = lines(leader.stdout.take().unwrap());
    let up = next(&from_group, "the grandchild").expect("the grandchild's first line");
    let grandchild = up
        .strip_prefix("up ")
        .expect("the grandchild says it is up");
    let grandchild_pid = grandchild
        .parse::<i32>()
        .expect("the grandchild says its pid");
    let start = aterm_spec::start::at_defaults(&aterm_spec::start::TERMINATING);
    let mut standin = command(&trustd)
        .args(["-c", "echo up; /bin/cat; echo \"cat $?\""])
        .process_group(group)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(start);
    let from_standin = lines(standin.stdout.take().unwrap());
    let mut to_standin = standin.stdin.take().unwrap();
    assert_eq!(next(&from_standin, "the stand-in").as_deref(), Some("up"));
    to_standin.write_all(b"before\n").unwrap();
    assert_eq!(
        next(&from_standin, "its cat").as_deref(),
        Some("before"),
        "the trustd's cat runs in the group"
    );

    let home = i32::try_from(std::process::id()).unwrap();
    let standin_pid = i32::try_from(standin.id()).unwrap();
    imp::signal_group_with(group, SIGTERM, &|| true, &|| {
        Some(vec![
            Proc {
                pid: group,
                parent: home,
                pgid: group,
                name: "sh".to_string(),
            },
            Proc {
                pid: grandchild_pid,
                parent: group,
                pgid: group,
                name: "sh".to_string(),
            },
            Proc {
                pid: standin_pid,
                parent: home,
                pgid: group,
                name: "trustd".to_string(),
            },
        ])
    });

    let status = ended(&mut leader, "the leader");
    assert_eq!(
        status.signal(),
        Some(SIGTERM),
        "the leader ends by the signal: {status}"
    );
    assert_eq!(
        next(&from_group, "the grandchild"),
        Some(format!("TERM {grandchild}")),
        "the grandchild ends by its own handler"
    );
    assert_eq!(
        next(&from_group, "the group's pipe"),
        None,
        "and nothing holds the pipe"
    );
    let wrote = to_standin.write_all(b"after\n").map_err(|e| e.kind());
    let echoed = next(&from_standin, "its cat");
    assert_eq!(
        (wrote, echoed.as_deref()),
        (Ok(()), Some("after")),
        "the trustd's cat echoes after the send"
    );
    drop(to_standin);
    assert_eq!(
        next(&from_standin, "its cat's end").as_deref(),
        Some("cat 0"),
        "its cat ends at its input's end, and the stand-in reports it"
    );
    let status = ended(&mut standin, "the stand-in");
    assert_eq!(
        status.code(),
        Some(0),
        "the trustd stand-in exits by itself: {status}"
    );
}
