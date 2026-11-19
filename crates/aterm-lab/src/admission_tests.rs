// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use std::path::PathBuf;
use std::time::Instant;

use aterm_capacity::{Fixed, Reading};
use aterm_spec::wait::{MUST_HAPPEN, until_within};

fn functional_need() -> Need {
    Need::new("functional fixture")
        .mem_gib(2)
        .margin(FUNCTIONAL_MARGIN)
}

#[test]
fn functional_claim_reserves_memory_without_timing_or_quiet_evidence() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let mut reading = Reading::of_gib(48, 40, 14);
    reading.load1 = Some(200.0);
    let fixed = Fixed::new(reading);
    let mut claim = functional_in(
        Some(&ledger),
        &fixed,
        functional_need(),
        "functional fixture",
        Admit::Wait,
        Pace::default(),
        &mut |line| panic!("unexpected wait: {line}"),
    )
    .unwrap();
    let records = ledger.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].need.mem, 2 * aterm_capacity::floor::GIB);
    assert_eq!(records[0].need.margin, FUNCTIONAL_MARGIN);
    assert!(records[0].need.quiet.is_none());
    assert!(!records[0].need.covers);
    assert!(!records[0].held.unwrap().alone);
    assert_eq!(
        claim.timing_admission(),
        Err(aterm_capacity::TimingIssue::UnknownAdmission)
    );
    assert_eq!(
        claim.measurement_entry(),
        Err(aterm_capacity::TimingIssue::UnknownAdmission)
    );
    assert!(fixed.cpu_point().is_none());
    drop(claim);
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn functional_claim_refuses_now_zero_missing_and_unknown_before_work() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let healthy = Fixed::new(Reading::of_gib(48, 40, 14));
    for (ledger, need, mode) in [
        (Some(&ledger), functional_need(), Admit::Now),
        (
            Some(&ledger),
            Need::new("zero").margin(FUNCTIONAL_MARGIN),
            Admit::Wait,
        ),
        (None, functional_need(), Admit::Wait),
    ] {
        let error = functional_in(
            ledger,
            &healthy,
            need,
            "functional fixture",
            mode,
            Pace::default(),
            &mut |line| panic!("unexpected wait: {line}"),
        )
        .unwrap_err();
        assert_eq!(error.code, 3);
        assert!(error.msg.unwrap().contains("started nothing"));
    }
    for reading in [
        Reading::default(),
        Reading {
            available: None,
            ..Reading::of_gib(48, 40, 14)
        },
        Reading {
            physical: None,
            ..Reading::of_gib(48, 40, 14)
        },
    ] {
        let error = functional_in(
            Some(&ledger),
            &Fixed::new(reading),
            functional_need(),
            "functional fixture",
            Admit::Wait,
            Pace::default(),
            &mut |line| panic!("unknown memory must not wait or admit: {line}"),
        )
        .unwrap_err();
        assert_eq!(error.code, 3);
        assert!(error.msg.unwrap().contains("memory observation"));
    }
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn non_timing_shortages_wait_for_actual_room_and_never_use_alone() {
    struct RoomAfterLine {
        ready: Cell<bool>,
        start: Instant,
    }
    impl Machine for RoomAfterLine {
        fn read(&self, _: Option<&Path>) -> Reading {
            assert!(self.start.elapsed() < MUST_HAPPEN, "admission hung");
            Reading::of_gib(48, if self.ready.get() { 40 } else { 0 }, 14)
        }
        fn procs(&self) -> Option<aterm_capacity::Procs> {
            None
        }
    }
    for purpose in [ResourcePurpose::Functional, ResourcePurpose::Correctness] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let machine = RoomAfterLine {
            ready: Cell::new(false),
            start: Instant::now(),
        };
        let claim = resource_in(
            purpose,
            Some(&ledger),
            &machine,
            purpose.need("window fixture".into(), 2 << 30),
            "window fixture",
            Admit::Wait,
            Pace {
                poll: std::time::Duration::ZERO,
                ..Pace::default()
            },
            &mut |line| {
                if machine.ready.get() {
                    assert!(line.contains("admitted"), "{line}");
                } else {
                    assert!(line.contains("waits") && line.contains("memory"), "{line}");
                    machine.ready.set(true);
                }
            },
        )
        .unwrap();
        assert!(
            machine.ready.get(),
            "a zero-room ALONE admission would skip the actual wait"
        );
        assert!(!ledger.list().unwrap()[0].held.unwrap().alone);
        drop(claim);
        assert!(ledger.list().unwrap().is_empty());
    }
}

#[test]
fn functional_memory_must_remain_known_during_admission() {
    struct Lost {
        reads: Cell<u32>,
    }
    impl Machine for Lost {
        fn read(&self, _: Option<&Path>) -> Reading {
            let reads = self.reads.get();
            self.reads.set(reads + 1);
            if reads == 0 {
                Reading::of_gib(48, 40, 14)
            } else {
                Reading::default()
            }
        }
        fn procs(&self) -> Option<aterm_capacity::Procs> {
            None
        }
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let machine = Lost {
        reads: Cell::new(0),
    };
    let error = functional_in(
        Some(&ledger),
        &machine,
        functional_need(),
        "functional fixture",
        Admit::Wait,
        Pace {
            poll: std::time::Duration::ZERO,
            ..Pace::default()
        },
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(error.code, 3);
    assert!(error.msg.unwrap().contains("changed to unknown"));
    assert!(machine.reads.get() > 1);
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn correctness_admission_ignores_cpu_load_without_issuing_timing_evidence() {
    for load in [Some(200.0), None] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let mut reading = Reading::of_gib(48, 40, 14);
        reading.load1 = load;
        let fixed = Fixed::new(reading);
        let purpose = ResourcePurpose::Correctness;
        let mut claim = resource_in(
            purpose,
            Some(&ledger),
            &fixed,
            purpose.need("correctness fixture".into(), 2 << 30),
            "correctness fixture",
            Admit::Wait,
            Pace::default(),
            &mut |_| {},
        )
        .unwrap();
        let records = ledger.list().unwrap();
        let held = &records[0];
        assert!(held.need.quiet.is_some());
        assert_eq!(held.need.quiet_policy, aterm_capacity::QuietPolicy::Load);
        assert!(held.need.patience.is_none());
        assert_eq!(held.need.mem, 2 << 30);
        assert_eq!(held.need.margin, FUNCTIONAL_MARGIN);
        assert!(!held.held.unwrap().alone);
        assert!(fixed.cpu_point().is_none());
        assert_eq!(
            claim.measurement_entry(),
            Err(aterm_capacity::TimingIssue::UnknownAdmission)
        );
        assert_eq!(
            claim.timing_admission(),
            Err(aterm_capacity::TimingIssue::UnknownAdmission)
        );
        drop(claim);
        let refused = quiet_in(
            Some(&ledger),
            &fixed,
            Need::timing("timing control"),
            "timing control",
            Admit::Wait,
            Pace::default(),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(refused.code, 3);
        assert!(ledger.list().unwrap().is_empty());
    }
}

#[test]
fn correctness_admission_refuses_invalid_or_unknown_resources() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let purpose = ResourcePurpose::Correctness;
    let need = purpose.need("correctness fixture".into(), 2 << 30);
    let healthy = Fixed::new(Reading::of_gib(48, 40, 14));
    for (ledger, need, mode) in [
        (Some(&ledger), need.clone(), Admit::Now),
        (Some(&ledger), purpose.need("zero".into(), 0), Admit::Wait),
        (
            Some(&ledger),
            Need::timing("wrong policy")
                .mem_gib(2)
                .margin(FUNCTIONAL_MARGIN),
            Admit::Wait,
        ),
        (None, need.clone(), Admit::Wait),
    ] {
        assert_eq!(
            resource_in(
                purpose,
                ledger,
                &healthy,
                need,
                "fixture",
                mode,
                Pace::default(),
                &mut |_| {}
            )
            .unwrap_err()
            .code,
            3
        );
    }
    for reading in [
        Reading::default(),
        Reading {
            available: None,
            ..Reading::of_gib(48, 40, 14)
        },
        Reading {
            physical: None,
            ..Reading::of_gib(48, 40, 14)
        },
    ] {
        assert_eq!(
            resource_in(
                purpose,
                Some(&ledger),
                &Fixed::new(reading),
                need.clone(),
                "fixture",
                Admit::Wait,
                Pace::default(),
                &mut |_| {}
            )
            .unwrap_err()
            .code,
            3
        );
        assert!(ledger.list().unwrap().is_empty());
    }
}

const CORRECTNESS_PEER: &str = "ATERM_LAB_CORRECTNESS_PEER";
const CORRECTNESS_PEER_KIND: &str = "ATERM_LAB_CORRECTNESS_PEER_KIND";

#[test]
fn standin_correctness_quiet_peer() {
    let Some(dir) = std::env::var_os(CORRECTNESS_PEER).map(PathBuf::from) else {
        return;
    };
    let ledger = Ledger::at(dir.join("ledger"));
    let machine = machine();
    let need = match std::env::var(CORRECTNESS_PEER_KIND).as_deref() {
        Ok("correctness") => ResourcePurpose::Correctness.need("correctness peer".into(), 2 << 30),
        Ok("timing") => Need::timing("timing peer"),
        _ => panic!("unknown peer purpose"),
    };
    let waiting = aterm_capacity::admit::Waiter::enter(&ledger, &machine, need).unwrap();
    let mut waiting = match waiting.poll(&machine) {
        aterm_capacity::admit::Polled::Waiting(waiting, short) => {
            assert!(short.shorts.iter().any(|short| matches!(
                short,
                aterm_capacity::admit::Short::Quiet { held: true, .. }
            )));
            Some(waiting)
        }
        other => panic!("independent window did not keep quiet exclusion: {other:?}"),
    };
    std::fs::write(dir.join("blocked"), b"blocked").unwrap();
    until_within(MUST_HAPPEN, "parent releases quiet claim", || {
        dir.join("release").exists().then_some(())
    });
    let claim = until_within(
        MUST_HAPPEN,
        "peer acquires released quiet claim",
        || match waiting.take().unwrap().poll(&machine) {
            aterm_capacity::admit::Polled::Admitted(claim) => Some(claim),
            aterm_capacity::admit::Polled::Waiting(next, _) => {
                waiting = Some(next);
                None
            }
            other => panic!("peer admission failed: {other:?}"),
        },
    );
    drop(claim);
}

#[test]
fn correctness_and_timing_exclude_each_other_across_processes() {
    for peer in ["timing", "correctness"] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(scratch.path().join("ledger"));
        let held = if peer == "timing" {
            resource_in(
                ResourcePurpose::Correctness,
                Some(&ledger),
                &machine(),
                ResourcePurpose::Correctness.need("correctness holder".into(), 2 << 30),
                "holder",
                Admit::Wait,
                Pace::default(),
                &mut |_| {},
            )
            .unwrap()
        } else {
            quiet_in(
                Some(&ledger),
                &machine(),
                Need::timing("timing holder"),
                "holder",
                Admit::Wait,
                Pace {
                    poll: std::time::Duration::ZERO,
                    ..Pace::default()
                },
                &mut |_| {},
            )
            .unwrap()
        };
        let mut child = Standin(
            aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "admission::tests::standin_correctness_quiet_peer",
                    "--test-threads=1",
                ])
                .env(CORRECTNESS_PEER, scratch.path())
                .env(CORRECTNESS_PEER_KIND, peer)
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .unwrap(),
        );
        until_within(MUST_HAPPEN, "independent quiet exclusion", || {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "peer exited before proving exclusion"
            );
            scratch.path().join("blocked").exists().then_some(())
        });
        assert!(child.0.try_wait().unwrap().is_none());
        assert_eq!(ledger.list().unwrap().len(), 2);
        drop(held);
        std::fs::write(scratch.path().join("release"), b"release").unwrap();
        let status = until_within(MUST_HAPPEN, "independent peer completed", || {
            child.0.try_wait().unwrap()
        });
        assert!(status.success(), "{peer}: {status}");
        assert!(ledger.list().unwrap().is_empty());
    }
}

#[test]
fn window_lane_reuses_original_run_but_blocks_an_independent_run() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let original = Mutex::new(None);
    let other = Mutex::new(None);
    for _ in 0..2 {
        window_lane_in(
            &original,
            Some(&ledger),
            "functional fixture",
            Wait::No,
            &mut |line| panic!("same run must not wait: {line}"),
            &|| false,
        )
        .unwrap();
    }
    assert_eq!(
        window_lane_in(
            &other,
            Some(&ledger),
            "timing fixture",
            Wait::No,
            &mut |_| {},
            &|| false
        )
        .unwrap_err()
        .code,
        3
    );
    assert!(other.lock().unwrap().is_none());
    assert!(
        ledger.list().unwrap().is_empty(),
        "lane is not a memory or Timing claim"
    );
    drop(original);
    // A sibling fork can retain the CLOEXEC description until exec. Wait for its final
    // close, as the record-lock tests do; dropping this guard is not a latency promise.
    let start = Instant::now();
    window_lane_in(
        &other,
        Some(&ledger),
        "timing fixture",
        Wait::Block,
        &mut |_| {},
        &|| start.elapsed() >= MUST_HAPPEN,
    )
    .unwrap();
}

#[test]
fn window_lane_fails_closed_on_missing_storage_replacement_and_stopping() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let run = Mutex::new(None);
    for (ledger, stopped) in [(None, false), (Some(&ledger), true)] {
        assert_eq!(
            window_lane_in(&run, ledger, "fixture", Wait::No, &mut |_| {}, &|| stopped)
                .unwrap_err()
                .code,
            3
        );
        assert!(run.lock().unwrap().is_none());
    }
    window_lane_in(
        &run,
        Some(&ledger),
        "fixture",
        Wait::No,
        &mut |_| {},
        &|| false,
    )
    .unwrap();
    let path = ledger.dir().join(WINDOW_LANE);
    std::fs::rename(&path, ledger.dir().join("original-window.lock")).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    assert_eq!(
        window_lane_in(
            &run,
            Some(&ledger),
            "fixture",
            Wait::No,
            &mut |_| {},
            &|| false
        )
        .unwrap_err()
        .code,
        3
    );
}

#[test]
fn occupied_window_lane_wait_can_be_interrupted_without_starting_work() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let holder = Mutex::new(None);
    window_lane_in(
        &holder,
        Some(&ledger),
        "holder",
        Wait::No,
        &mut |_| {},
        &|| false,
    )
    .unwrap();
    let waiter = Mutex::new(None);
    let stop = Cell::new(false);
    let start = Instant::now();
    let error = window_lane_in(
        &waiter,
        Some(&ledger),
        "waiter",
        Wait::Block,
        &mut |line| {
            assert!(line.contains("account window lane"));
            stop.set(true);
        },
        &|| {
            assert!(start.elapsed() < MUST_HAPPEN, "lane wait hung");
            stop.get()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, 3);
    assert!(stop.get());
    assert!(waiter.lock().unwrap().is_none());
    assert!(holder.lock().unwrap().is_some());
}

/// Set, the test binary is [`standin_waits_for_the_quiet_machine`], its ledger under this
/// directory.
const STANDIN: &str = "ATERM_LAB_QUIET_INTERRUPT_STANDIN";

/// The machine both sides read: idle, so only the quiet claim the test holds keeps the
/// stand-in waiting.
struct Measured {
    fixed: Fixed,
    origin: Instant,
    points: std::cell::Cell<u64>,
}
impl Machine for Measured {
    fn read(&self, path: Option<&std::path::Path>) -> Reading {
        self.fixed.read(path)
    }
    fn procs(&self) -> Option<aterm_capacity::Procs> {
        self.fixed.procs()
    }
    fn cpu_point(&self) -> Option<aterm_capacity::CpuPoint> {
        let n = self.points.get() + 1;
        self.points.set(n);
        Some(aterm_capacity::CpuPoint {
            at: self.origin + std::time::Duration::from_secs(n),
            unix_ms: aterm_time::unix_millis()?,
            ticks: vec![aterm_sysprobe::CoreTicks {
                user: n,
                idle: n * 99,
                ..Default::default()
            }],
        })
    }
}
fn machine() -> Measured {
    Measured {
        fixed: Fixed::new(Reading::of_gib(48, 40, 14)),
        origin: Instant::now(),
        points: std::cell::Cell::new(0),
    }
}

#[test]
fn requested_timing_refuses_unknown_now_and_missing_ledger_before_work() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let fixed = Fixed::new(Reading::of_gib(48, 40, 14));
    for (ledger, mode) in [
        (Some(&ledger), Admit::Wait),
        (Some(&ledger), Admit::Now),
        (None, Admit::Wait),
    ] {
        let refused = quiet_in(
            ledger,
            &fixed,
            Need::timing("refusal control"),
            "refusal control",
            mode,
            Pace::default(),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(refused.code, 3);
        assert!(refused.msg.unwrap().contains("started nothing"));
    }
    let claim = quiet_in(
        Some(&ledger),
        &machine(),
        Need::timing("positive measured fixture"),
        "positive measured fixture",
        Admit::Wait,
        Pace {
            poll: std::time::Duration::ZERO,
            ..Pace::default()
        },
        &mut |_| {},
    )
    .unwrap();
    assert!(claim.timing_admission().is_ok());
    // The positive fixture remains explicit; Fixed itself still supplies no CPU proof.
    assert!(fixed.cpu_point().is_none());
}

/// The stand-in's quiet claim for the x86_64 GUI rows, looking every two minutes (twice the
/// parent's hang detector, so only an interrupt ends the wait in time).
fn gui_rows(dir: &std::path::Path, say: &mut dyn FnMut(&str)) -> Result<Claim, Fail> {
    quiet_in(
        Some(&Ledger::at(dir.join("ledger"))),
        &machine(),
        Need::timing("lab proof: x86_64 GUI rows"),
        "x86_64 GUI rows",
        Admit::Wait,
        Pace {
            poll: MUST_HAPPEN * 2,
            ..Pace::default()
        },
        say,
    )
}

/// Run by [`an_interrupt_ends_the_runs_quiet_claims`] in a process of its own, its
/// interrupts the lab's own ([`crate::stop::install`]) with SIGINT and SIGTERM at their
/// default actions, as a foreground run has them: waits for the quiet machine, writing `waiting` at its first
/// wait line; once an interrupt ends that wait, a later quiet claim of the run is refused
/// without a wait, and it exits as the lab's main would ([`crate::stop::status`]), saying
/// what a FAIL found before, and a refusal, would have exited.
#[test]
fn standin_waits_for_the_quiet_machine() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    crate::sys::start_signal(libc::SIGINT, false);
    // SIGTERM starts at its default, set here, not this test binary's: `install` leaves an
    // ignored one ignored, and the SIGTERM leg sends it.
    crate::sys::start_signal(libc::SIGTERM, false);
    crate::stop::install();
    let waiting = dir.join("waiting");
    let first = gui_rows(&dir, &mut |line| {
        if !waiting.exists() {
            std::fs::write(&waiting, line).expect("said waiting");
        }
    })
    .expect_err("an interrupt ended the wait");
    assert!(
        crate::stop::ended_a_wait(),
        "the wait handed its interrupt on"
    );
    let sig = first.stopped.expect("the interrupt's signal");
    assert_eq!(crate::stop::check(), Err(crate::stop::Stopped(sig)));
    let again = gui_rows(&dir, &mut |line| panic!("a second wait: {line}"))
        .expect_err("no quiet claim once the run is ending");
    println!("{}", first.msg.unwrap_or_default());
    println!("{}", again.msg.unwrap_or_default());
    println!(
        "a FAIL found before exits {}, a refusal {}",
        crate::stop::status(1, true),
        crate::stop::status(1, false)
    );
    std::process::exit(crate::stop::status(first.code, false));
}

/// A stand-in, killed when it is dropped still running (a test that failed before it
/// ended), so none outlives its test.
struct Standin(aterm_uds::spawnfd::Child);

impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// The lab's quiet claim (a windowed instance's, the Linux lane's GUI rows') ends on SIGTERM,
/// or SIGINT, as NOT RUN (exit 3) well inside the one-minute hang detector (the time is
/// reported), naming the signal; the interrupt becomes the run's, so its next quiet claim is
/// refused without a wait, and a FAIL the verb found before stays exit 1 (a refusal's exit 1
/// does not). Only the test's claim is left in the ledger.
#[test]
fn an_interrupt_ends_the_runs_quiet_claims() {
    for (sig, word) in [(libc::SIGTERM, "SIGTERM"), (libc::SIGINT, "SIGINT")] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("lab-quiet-int-")
            .tempdir()
            .expect("a scratch ledger");
        let ledger = Ledger::at(dir.path().join("ledger"));
        let held = aterm_capacity::admit_with(
            &ledger,
            &machine(),
            Need::new("holder").quiet(28.0),
            Admit::Wait,
            Pace::default(),
            &mut |_| {},
        )
        .expect("the holder's claim");
        let mut child = Standin(
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "admission::tests::standin_waits_for_the_quiet_machine",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(STANDIN, dir.path())
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("the stand-in"),
        );
        until_within(MUST_HAPPEN, "the stand-in's wait", || {
            std::fs::metadata(dir.path().join("waiting")).ok()
        });
        let pid = i32::try_from(child.0.id()).expect("a pid");
        let sent = Instant::now();
        crate::sys::kill(pid, sig).expect("signalled the stand-in");
        let status = until_within(MUST_HAPPEN, "the stand-in's end", || {
            child.0.try_wait().ok().flatten()
        });
        let took = sent.elapsed();
        let mut out = String::new();
        if let Some(mut o) = child.0.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut o, &mut out);
        }
        assert_eq!(status.code(), Some(3), "{word}: {status}\n{out}");
        assert!(
            out.contains(&format!(
                "NOT RUN: x86_64 GUI rows: its quiet claim was interrupted ({word}) while it \
                 waited"
            )),
            "{word}: {out}"
        );
        assert!(
            out.contains("NOT RUN: x86_64 GUI rows: its quiet claim was not asked for"),
            "{word}: {out}"
        );
        assert!(
            out.contains("a FAIL found before exits 1, a refusal 3"),
            "{word}: {out}"
        );
        let left: Vec<String> = ledger
            .dir()
            .read_dir()
            .expect("the ledger")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".claim") || n.ends_with(".wait"))
            .collect();
        assert_eq!(left.len(), 1, "{word}: only the holder's claim: {left:?}");
        eprintln!("{word} ended the lab's quiet claim in {took:?}");
        drop(held);
    }
}

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(|w| (*w).to_owned()).collect()
}

/// `--admit` is taken out wherever it stands before a `--`, and a word after `--` belongs to
/// the program the verb runs.
#[test]
fn admit_is_taken_out_before_the_verb_reads_its_arguments() {
    let mut a = words(&["--window", "--admit", "now", "--trials", "3"]);
    assert_eq!(strip(&mut a).ok(), Some(Admit::Now));
    assert_eq!(a, words(&["--window", "--trials", "3"]));
    let mut a = words(&["--window"]);
    assert_eq!(strip(&mut a).ok(), Some(Admit::Wait));
    assert_eq!(a, words(&["--window"]));
    let mut a = words(&["--", "prog", "--admit", "now"]);
    assert_eq!(strip(&mut a).ok(), Some(Admit::Wait));
    assert_eq!(a.len(), 4, "the program's own words stay");
    let mut a = words(&["--admit=now", "proof"]);
    assert_eq!(strip(&mut a).ok(), Some(Admit::Now));
    assert_eq!(a, words(&["proof"]));
    assert!(strip(&mut words(&["--admit=soon"])).is_err());
    assert!(strip(&mut words(&["--admit", "soon"])).is_err());
    assert!(strip(&mut words(&["--admit"])).is_err());
    assert!(strip(&mut words(&["--admit", "--", "now"])).is_err());
}

#[test]
fn independent_comparison_owners_serialize_until_the_original_guard_releases() {
    let slot = Mutex::new(());
    let first = comparison_slot(&slot).unwrap();
    assert!(
        comparison_slot(&slot).is_err(),
        "a second quad cannot acquire independent launch authority"
    );
    drop(first);
    let next = comparison_slot(&slot).unwrap();
    assert!(comparison_slot(&slot).is_err());
    drop(next);
    assert!(
        comparison_slot(&slot).is_ok(),
        "release permits a fresh quad to requeue normally"
    );
}

fn reported_need() -> Need {
    Need::timing("reported GUI fixture").patience(aterm_capacity::Patience {
        reserve: std::time::Duration::ZERO,
        start: std::time::Duration::ZERO,
        hold: MUST_HAPPEN,
    })
}

fn loaded_machine() -> Measured {
    let mut reading = Reading::of_gib(48, 40, 14);
    reading.load1 = Some(200.0);
    Measured {
        fixed: Fixed::new(reading),
        ..machine()
    }
}

#[test]
fn reported_timing_admits_loaded_once_and_retains_the_original_exclusion() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let mut work = 0;
    let admitted = reported_timing_in(
        Some(&ledger),
        &loaded_machine(),
        reported_need(),
        "reported GUI fixture",
        Admit::Wait,
        Pace {
            poll: std::time::Duration::ZERO,
            ..Pace::default()
        },
        &mut |_| {},
    )
    .inspect(|_| {
        work += 1;
    })
    .unwrap();
    assert_eq!(work, 1);
    let aterm_capacity::Protection::Loaded(observed) = admitted.protection() else {
        panic!("busy admission must retain its loaded observation");
    };
    assert_eq!(observed.load1, 200.0);
    assert!(observed.load1 > observed.max);
    assert!(observed.window.interval_ns > 0);
    assert_eq!(
        admitted._claim.timing_admission(),
        Err(aterm_capacity::TimingIssue::Loaded)
    );
    let records = ledger.list().unwrap();
    assert_eq!(
        records.len(),
        1,
        "one original resource claim, never a diagnostic substitute"
    );
    assert!(records[0].held.is_some());
    assert!(records[0].need.quiet.is_some());
    assert_eq!(records[0].timing.cause, aterm_capacity::TimingCause::Loaded);
    drop(admitted);
    assert!(
        ledger.list().unwrap().is_empty(),
        "the scoped owner releases exclusion"
    );

    // The strict validator rejects the same actual loaded admission even when its resource
    // request would permit running loaded. Replacing that validator with protection breaks this.
    let refused = quiet_in(
        Some(&ledger),
        &loaded_machine(),
        reported_need().runs_loaded(),
        "strict GUI fixture",
        Admit::Wait,
        Pace {
            poll: std::time::Duration::ZERO,
            ..Pace::default()
        },
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(refused.code, crate::linux::NOT_RUN);
    assert!(ledger.list().unwrap().is_empty());
    assert_eq!(work, 1);
}

#[test]
fn reported_timing_keeps_quiet_admission_and_refuses_unread_or_forced_evidence() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let pace = Pace {
        poll: std::time::Duration::ZERO,
        ..Pace::default()
    };
    let admitted = reported_timing_in(
        Some(&ledger),
        &machine(),
        reported_need(),
        "quiet reported fixture",
        Admit::Wait,
        pace,
        &mut |_| {},
    )
    .unwrap();
    assert!(
        matches!(admitted.protection(), aterm_capacity::Protection::Qualified(window) if window.ready())
    );
    assert!(admitted._claim.timing_admission().is_ok());
    drop(admitted);
    assert!(ledger.list().unwrap().is_empty());

    struct FrozenCpu(Measured);
    impl Machine for FrozenCpu {
        fn read(&self, path: Option<&Path>) -> Reading {
            self.0.read(path)
        }
        fn procs(&self) -> Option<aterm_capacity::Procs> {
            self.0.procs()
        }
        fn cpu_point(&self) -> Option<aterm_capacity::CpuPoint> {
            let mut point = self.0.cpu_point()?;
            for ticks in &mut point.ticks {
                *ticks = Default::default();
            }
            Some(point)
        }
    }
    let blind = FrozenCpu(machine());
    let unread_cpu = Fixed::new(Reading::of_gib(48, 40, 14));
    let mut reading = Reading::of_gib(48, 40, 14);
    reading.load1 = None;
    let unread_load = Measured {
        fixed: Fixed::new(reading),
        ..machine()
    };
    let measured = machine();
    for (book, source, mode) in [
        (Some(&ledger), &blind as &dyn Machine, Admit::Wait),
        (Some(&ledger), &unread_cpu as &dyn Machine, Admit::Wait),
        (Some(&ledger), &unread_load as &dyn Machine, Admit::Wait),
        (Some(&ledger), &measured as &dyn Machine, Admit::Now),
        (None, &measured as &dyn Machine, Admit::Wait),
    ] {
        let mut work = 0;
        let refused = reported_timing_in(
            book,
            source,
            reported_need(),
            "unread reported fixture",
            mode,
            pace,
            &mut |_| {},
        )
        .inspect(|_| {
            work += 1;
        })
        .unwrap_err();
        assert_eq!(refused.code, crate::linux::NOT_RUN);
        assert_eq!(
            work, 0,
            "unknown observations never authorize diagnostic work"
        );
        assert!(ledger.list().unwrap().is_empty());
    }
    let expired = reported_need().patience(aterm_capacity::Patience {
        reserve: std::time::Duration::ZERO,
        start: std::time::Duration::ZERO,
        hold: std::time::Duration::ZERO,
    });
    let refused = reported_timing_in(
        Some(&ledger),
        &machine(),
        expired,
        "expired reported fixture",
        Admit::Wait,
        pace,
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(refused.code, crate::linux::NOT_RUN);
    assert!(ledger.list().unwrap().is_empty());
}
