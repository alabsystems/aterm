// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_spec::wait::{MUST_HAPPEN, until_within};
use std::path::PathBuf;
use std::time::Duration;

/// The stand-in's scratch folder: set only in the stand-in process ([`stand_in_holds_a_gate`]).
const STAND_IN: &str = "ATPKG_GATE_STAND_IN";
/// The gate the stand-in holds.
const STAND_IN_GATE: &str = "ATPKG_GATE_STAND_IN_GATE";
/// How the stand-in holds it: `record`, through this module; `raw`, as an atpkg from before the
/// record does, writing nothing; `stale`, writing its line as this module does but leaving it
/// there as it lets go, and running on until `end` stands.
const STAND_IN_MODE: &str = "ATPKG_GATE_STAND_IN_MODE";

/// Existence is the reader's readiness event: only complete bytes are published.
fn publish(path: &Path, text: &str) {
    aterm_uds::atomic_file::write_flushed(path, text.as_bytes(), 0o600, None).unwrap();
}

/// A SECOND PROCESS THAT HOLDS A GATE: this test binary, started by [`stand_in`]. It holds the
/// gate, says its pid in `ready`, and lets go once `release` stands (or the hang detector passed).
#[test]
#[ignore = "the second process the gate tests start"]
fn stand_in_holds_a_gate() {
    let Some(dir) = std::env::var_os(STAND_IN).map(PathBuf::from) else {
        return;
    };
    let gate = PathBuf::from(std::env::var_os(STAND_IN_GATE).unwrap());
    let file = open(&gate).unwrap();
    let mode = std::env::var(STAND_IN_MODE).unwrap();
    let held: Box<dyn std::any::Any> = match mode.as_str() {
        "raw" => Box::new(crate::lock::Flock::try_lock(file).unwrap()),
        "stale" => {
            use std::os::unix::fs::FileExt as _;
            let lock = crate::lock::Flock::try_lock(file).unwrap();
            lock.write_all_at(line("exclusive").unwrap().as_bytes(), 0)
                .unwrap();
            Box::new(lock)
        }
        _ => Box::new(try_exclusive(file).unwrap()),
    };
    publish(&dir.join("ready"), &std::process::id().to_string());
    until_within(MUST_HAPPEN, "the test let the stand-in go", || {
        dir.join("release").exists().then_some(())
    });
    drop(held);
    if mode == "stale" {
        publish(&dir.join("released"), "released");
        until_within(MUST_HAPPEN, "the test ended the stand-in", || {
            dir.join("end").exists().then_some(())
        });
    }
}

/// A running [`stand_in_holds_a_gate`], killed and reaped once dropped.
struct StandIn {
    child: aterm_uds::spawnfd::Child,
    pid: i32,
}

impl StandIn {
    /// End it as a killed holder ends: its record is never taken back.
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    /// Whether it still runs: it has not exited, read without reaping it.
    fn runs(&mut self) -> bool {
        matches!(self.child.exited(), Ok(false))
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start [`stand_in_holds_a_gate`] on `gate` in `mode`, its markers in `dir`, and wait until it
/// holds the gate.
fn stand_in(dir: &Path, gate: &Path, mode: &str) -> StandIn {
    std::fs::create_dir_all(dir).unwrap();
    let child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "build_cache::registered::gate::tests::stand_in_holds_a_gate",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(STAND_IN, dir)
        .env(STAND_IN_GATE, gate)
        .env(STAND_IN_MODE, mode)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap();
    let pid = until_within(MUST_HAPPEN, "the stand-in holds the gate", || {
        std::fs::read_to_string(dir.join("ready"))
            .ok()?
            .parse()
            .ok()
    });
    StandIn { child, pid }
}

/// An index's gates folder with one gate, as a producer finds them.
struct Gates {
    scratch: aterm_tempfile::TempDir,
    index: PathBuf,
    gate: PathBuf,
    root: PathBuf,
}

impl Gates {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt as _;
        let scratch = aterm_tempfile::tempdir().unwrap();
        // Spelled canonically, as the index is: a gate opens only in a folder named so.
        let at = scratch.path().canonicalize().unwrap();
        let index = at.join("index");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(index.join("locks.noindex"))
            .unwrap();
        let gate = index.join("locks.noindex/root.lock");
        drop(super::super::open_gate(&gate).unwrap());
        Self {
            root: at.join("root"),
            scratch,
            index,
            gate,
        }
    }

    /// A folder of its own for the markers of one stand-in.
    fn markers(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    /// The producer's take of the gate, under the admission lock, waiting until `within` passes
    /// with no work done by the holders on record, their work read as a producer reads it.
    fn produce(&self, within: Duration) -> std::io::Result<(crate::lock::Flock, Held)> {
        self.produce_reading(within, &mut work)
    }

    /// [`Self::produce`], each holder's work read by `work`.
    fn produce_reading(
        &self,
        within: Duration,
        work: &mut dyn FnMut(i32) -> Option<u64>,
    ) -> std::io::Result<(crate::lock::Flock, Held)> {
        let admission =
            crate::lock::Flock::lock(super::super::open_admission(&self.index).unwrap()).unwrap();
        let gate = super::super::open_gate(&self.gate).unwrap();
        super::super::producer_gate(
            &self.index,
            admission,
            gate,
            &self.gate,
            &self.root,
            within,
            work,
        )
    }

    /// The producer's refusal: [`Self::produce`] that did not take the gate.
    fn refused(&self, within: Duration) -> std::io::Error {
        self.produce(within)
            .err()
            .expect("the producer was refused the gate")
    }
}

/// The file name a stand-in's command line begins with: this test binary's.
fn program() -> String {
    std::env::current_exe()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// A second process that holds the gate is named in the producer's refusal by its pid and its
/// command; killed, it leaves its line, which then names a holder that has ended and never the
/// holder; and a second process that holds it as an atpkg from before the record does, writing
/// nothing, is told apart from both.
#[test]
fn a_second_process_holding_the_gate_is_named_and_one_that_ended_is_not() {
    let gates = Gates::new();
    let held = stand_in(&gates.markers("record"), &gates.gate, "record");
    let error = gates.refused(Duration::ZERO);
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock, "{error}");
    let words = error.to_string();
    assert!(words.contains(&gates.root.display().to_string()), "{words}");
    assert!(
        words.contains(&format!("held by pid {} ({} --exact ", held.pid, program())),
        "the refusal names the holder's pid and command: {words}"
    );
    assert!(words.contains("stand_in_holds_a_gate"), "{words}");

    let pid = held.pid;
    held.kill();
    // This process holds it now, as an atpkg from before the record would: a lock and no line.
    let raw = crate::lock::Flock::try_lock(crate::metadata_io::open_regular(&gates.gate).unwrap())
        .unwrap();
    let error = gates.refused(Duration::ZERO);
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock, "{error}");
    let words = error.to_string();
    assert!(
        !words.contains("held by"),
        "a holder that ended is not named as the holder: {words}"
    );
    assert!(
        words.contains(&format!("the holder on record, pid {pid} ("))
            && words.contains("has ended"),
        "{words}"
    );
    drop(raw);

    // A holder of this module takes the gate whole: the dead line goes, and so does its own.
    drop(gates.produce(Duration::ZERO).unwrap());
    assert_eq!(std::fs::read(&gates.gate).unwrap(), b"");

    let old = stand_in(&gates.markers("raw"), &gates.gate, "raw");
    let error = gates.refused(Duration::ZERO);
    let words = error.to_string();
    // What the record says comes first; the descriptor scan's evidence follows the gate's name.
    let (record, opened) = words
        .split_once("; gate=")
        .expect("the refusal names the gate after the record's words");
    assert!(
        record.contains("no holder is on record") && !record.contains(&format!("pid {}", old.pid)),
        "{words}"
    );
    // The holder that recorded nothing is still named, by its open descriptor on the gate.
    assert!(
        opened.contains("(not proof of lock ownership)")
            && opened.contains(&format!("pid {} (", old.pid)),
        "{words}"
    );
}

/// A producer that finds the gate held by a running holder on record in another process waits
/// for it, letting the admission lock go at each pause, and takes the gate once it lets go.
/// NEGATIVE CONTROL: a second process that holds it and recorded nothing is not waited for: it is
/// looked at once more after one pause, and refused.
#[test]
fn a_producer_waits_for_a_running_holder_on_record_and_not_for_one_unrecorded() {
    use std::cell::Cell;
    use std::rc::Rc;
    let gates = Gates::new();
    let markers = gates.markers("record");
    let held = stand_in(&markers, &gates.gate, "record");
    let waits = Rc::new(Cell::new(0_u32));
    let admission_free = Rc::new(Cell::new(true));
    let _hook = seam::on_waiting({
        let (waits, admission_free) = (Rc::clone(&waits), Rc::clone(&admission_free));
        let index = gates.index.clone();
        move |_| {
            waits.set(waits.get() + 1);
            let admission = super::super::open_admission(&index).unwrap();
            admission_free
                .set(admission_free.get() && crate::lock::Flock::try_lock(admission).is_ok());
            if waits.get() == 1 {
                publish(&markers.join("release"), "go");
            }
        }
    });
    let taken = gates.produce(MUST_HAPPEN);
    assert!(
        taken.is_ok(),
        "the producer waited for the running holder and took the gate: {:?}",
        taken.as_ref().err()
    );
    let (_admission, _gate) = taken.unwrap();
    assert!(waits.get() >= 1, "the producer waited for the holder");
    assert!(
        admission_free.get(),
        "no producer of another root waits behind this one's wait"
    );
    let me = holders(&gates.gate);
    assert_eq!(me.running.len(), 1, "{me:?}");
    assert_eq!(
        i32::try_from(std::process::id()).ok(),
        Some(me.running[0].pid)
    );
    drop((_admission, _gate));
    drop(held);

    waits.set(0);
    let _old = stand_in(&gates.markers("raw"), &gates.gate, "raw");
    let error = gates.refused(MUST_HAPPEN);
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock, "{error}");
    assert_eq!(
        waits.get(),
        1,
        "an unrecorded holder is looked at once more after one pause, never waited for"
    );
    assert!(
        error.to_string().contains("no holder is on record"),
        "{error}"
    );
    assert!(
        error.to_string().contains("after a wait of"),
        "a refusal says how long it waited wherever it paused, whoever its last read names: {error}"
    );
}

/// A producer waits for a running holder on record while that holder works, however long past
/// the window that takes, and takes the gate once it is let go. NEGATIVE CONTROLS: a holder that
/// does no work, and one whose work does not read, are waited for one window and no longer, and
/// the refusal says so.
#[test]
fn a_producer_waits_while_its_holder_works_and_no_longer_than_a_window_without_work() {
    use std::cell::Cell;
    use std::rc::Rc;
    const WINDOW: Duration = Duration::from_millis(300);
    let gates = Gates::new();
    let markers = gates.markers("working");
    let held = stand_in(&markers, &gates.gate, "record");
    let began = std::time::Instant::now();
    let _hook = seam::on_waiting({
        let markers = markers.clone();
        // Let the holder go only once the wait has outlasted three windows.
        move |_| {
            if began.elapsed() > WINDOW * 3 {
                publish(&markers.join("release"), "go");
            }
        }
    });
    let worked = Cell::new(0_u64);
    let taken = gates.produce_reading(WINDOW, &mut |pid| {
        (pid == held.pid).then(|| {
            worked.set(worked.get() + 1);
            worked.get()
        })
    });
    assert!(
        taken.is_ok(),
        "the producer waited for its working holder past the window: {:?}",
        taken.as_ref().err()
    );
    assert!(began.elapsed() > WINDOW * 3, "{:?}", began.elapsed());
    drop(taken);
    drop(held);

    for (name, reading) in [("idle", Some(7)), ("unread", None)] {
        let held = stand_in(&gates.markers(name), &gates.gate, "record");
        let pauses = Rc::new(Cell::new(0_u32));
        let _hook = seam::on_waiting({
            let pauses = Rc::clone(&pauses);
            move |_| pauses.set(pauses.get() + 1)
        });
        let began = std::time::Instant::now();
        let refused = gates
            .produce_reading(WINDOW, &mut |_| reading)
            .err()
            .unwrap_or_else(|| panic!("{name}: the producer was refused the gate"));
        assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock, "{refused}");
        assert!(began.elapsed() >= WINDOW, "{name}: {:?}", began.elapsed());
        assert!(pauses.get() >= 1, "{name}: it waited");
        let words = refused.to_string();
        assert!(
            words.contains(&format!("held by pid {} (", held.pid))
                && words.contains("no holder on record did any work this process could read"),
            "{name}: {words}"
        );
    }
}

/// A holder's work is the CPU time its process spent: it reads for this process, and grows as
/// this process works. A pid no process can have reads as none.
#[test]
fn a_holders_work_is_its_cpu_time_and_grows_as_it_works() {
    let me = i32::try_from(std::process::id()).unwrap();
    let before = work(me).expect("this process's work reads");
    let mut spun = 0_u64;
    until_within(MUST_HAPPEN, "this process's work grew", || {
        for step in 0..100_000_u64 {
            spun = std::hint::black_box(spun.wrapping_mul(31).wrapping_add(step));
        }
        work(me).filter(|now| *now > before)
    });
    assert_eq!(work(i32::MAX), None, "past every pid the kernel gives");
}

/// A holder of this module is off the record for a moment as it takes the gate and as it lets
/// go: a producer that meets the gate held with no holder on record looks again after one pause
/// and takes the gate where it is free by then. Here a second process that recorded nothing holds
/// it, and ends during that pause.
#[test]
fn a_producer_looks_again_after_a_pause_at_a_gate_held_with_no_holder_on_record() {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    let gates = Gates::new();
    let held = Rc::new(RefCell::new(Some(stand_in(
        &gates.markers("raw"),
        &gates.gate,
        "raw",
    ))));
    let waits = Rc::new(Cell::new(0_u32));
    let _hook = seam::on_waiting({
        let (held, waits) = (Rc::clone(&held), Rc::clone(&waits));
        move |_| {
            waits.set(waits.get() + 1);
            if let Some(held) = held.borrow_mut().take() {
                held.kill();
            }
        }
    });
    let taken = gates.produce(MUST_HAPPEN);
    assert!(
        taken.is_ok(),
        "the producer looked again after a pause and took the gate: {:?}",
        taken.as_ref().err()
    );
    assert_eq!(waits.get(), 1, "one pause, no wait for a holder");
}

/// A record can name a running holder after that holder let the gate go (its line could not be
/// taken back): a producer waiting for it looks at the gate itself at each pause, and takes it at
/// the first look that finds it free, not once its wait has run out.
#[test]
fn a_producer_takes_a_gate_found_free_though_its_record_still_names_a_running_holder() {
    use std::cell::Cell;
    use std::rc::Rc;
    let gates = Gates::new();
    let markers = gates.markers("stale");
    let mut held = stand_in(&markers, &gates.gate, "stale");
    assert!(holders(&gates.gate).awaited(), "the stand-in is on record");
    let waits = Rc::new(Cell::new(0_u32));
    let _hook = seam::on_waiting({
        let (waits, markers) = (Rc::clone(&waits), markers.clone());
        move |_| {
            waits.set(waits.get() + 1);
            if waits.get() == 1 {
                publish(&markers.join("release"), "go");
                until_within(MUST_HAPPEN, "the stand-in let the gate go", || {
                    markers.join("released").exists().then_some(())
                });
            }
        }
    });
    let taken = gates.produce(MUST_HAPPEN);
    assert!(taken.is_ok(), "{:?}", taken.as_ref().err());
    assert!(
        held.runs(),
        "the holder its record names still runs as the gate is taken"
    );
    assert_eq!(
        waits.get(),
        1,
        "the producer took the gate at its first look after the holder let it go"
    );
}

/// A producer waiting for a running holder on record holds no admission lock across its wait, and
/// takes it again only once the gate is free: another taker of that lock (a quiet build's
/// preparation, a retention) holds it from the producer's first pause, and the producer goes on
/// pausing for its holder meanwhile, then takes the gate once the holder has ended and that taker
/// let go.
#[test]
fn a_waiting_producer_takes_no_admission_lock_until_the_gate_is_free() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    let gates = Gates::new();
    let held = Rc::new(RefCell::new(Some(stand_in(
        &gates.markers("record"),
        &gates.gate,
        "record",
    ))));
    let pauses = Arc::new(AtomicU32::new(0));
    let other_holds = Arc::new(AtomicBool::new(false));
    let other = std::thread::spawn({
        let (pauses, other_holds, index) = (
            Arc::clone(&pauses),
            Arc::clone(&other_holds),
            gates.index.clone(),
        );
        move || {
            until_within(MUST_HAPPEN, "the producer paused", || {
                (pauses.load(Ordering::SeqCst) >= 1).then_some(())
            });
            let admission =
                crate::lock::Flock::lock(super::super::open_admission(&index).unwrap()).unwrap();
            other_holds.store(true, Ordering::SeqCst);
            // Not a hang detector that fails: a producer that blocks on this lock stops pausing,
            // and the test then reads that it did.
            let paused_on =
                aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
                    (pauses.load(Ordering::SeqCst) >= 3).then_some(())
                });
            drop(admission);
            paused_on.is_some()
        }
    });
    let _hook = seam::on_waiting({
        let (pauses, other_holds, held) = (
            Arc::clone(&pauses),
            Arc::clone(&other_holds),
            Rc::clone(&held),
        );
        move |_| {
            let now = pauses.fetch_add(1, Ordering::SeqCst) + 1;
            if now == 1 {
                until_within(
                    MUST_HAPPEN,
                    "the other taker holds the admission lock",
                    || other_holds.load(Ordering::SeqCst).then_some(()),
                );
            }
            if now == 3
                && let Some(held) = held.borrow_mut().take()
            {
                held.kill();
            }
        }
    });
    let taken = gates.produce(MUST_HAPPEN);
    assert!(
        other.join().unwrap(),
        "the producer paused for its holder twice more while another taker held the admission lock"
    );
    assert!(
        taken.is_ok(),
        "the producer took the gate once its holder ended: {:?}",
        taken.as_ref().err()
    );
}

/// A gate only ever read keeps the spaces its readers left until a holder of the whole gate
/// empties it: a reader that finds [`FULL`] bytes there and nobody holding the gate empties it
/// first, and is on record. NEGATIVE CONTROL: while another holds the gate, a full record is never
/// emptied, and the reader that finds it so is left off it.
#[test]
fn a_reader_empties_a_full_record_only_where_nobody_holds_the_gate() {
    let gates = Gates::new();
    let len = || usize::try_from(std::fs::metadata(&gates.gate).unwrap().len()).unwrap();
    let fill = || {
        while len() < FULL {
            drop(try_shared(open(&gates.gate).unwrap()).unwrap());
        }
    };
    fill();
    let reader = try_shared(open(&gates.gate).unwrap()).unwrap();
    assert_eq!(
        holders(&gates.gate).running.len(),
        1,
        "a reader that found the record full and the gate free emptied it and is on record"
    );
    assert!(len() < FULL, "{}", len());

    fill();
    let late = try_shared(open(&gates.gate).unwrap()).unwrap();
    assert!(
        len() >= FULL,
        "a full record another holder holds the gate beside is never emptied: {}",
        len()
    );
    assert!(
        std::fs::read_to_string(&gates.gate)
            .unwrap()
            .starts_with("holder pid="),
        "the holder's own line stays"
    );
    drop((late, reader));
}

/// A reader never refuses another reader: a hold of the whole gate that writes no record (another
/// reader's emptying of a full record, or a waiting producer's look, [`free`]) is ridden out, and
/// the reader takes the gate once it is let go, beside a line of a holder of the whole gate that
/// has ended too. NEGATIVE CONTROLS: a running holder of the whole gate on record refuses a reader
/// at once, with no pause, and a hold that writes no record still refuses it once [`SETTLE`] has
/// passed.
#[test]
fn a_reader_rides_out_a_hold_of_the_whole_gate_that_writes_no_record() {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    let gates = Gates::new();
    let paused = Rc::new(Cell::new(0_u32));
    let bare = Rc::new(RefCell::new(Some(
        crate::lock::Flock::try_lock(open(&gates.gate).unwrap()).unwrap(),
    )));
    let _hook = seam::on_settling({
        let (paused, bare) = (Rc::clone(&paused), Rc::clone(&bare));
        move || {
            paused.set(paused.get() + 1);
            // The hold lets go during the reader's first pause.
            drop(bare.borrow_mut().take());
        }
    });
    let reader = try_shared(open(&gates.gate).unwrap())
        .expect("a reader rides out a hold of the whole gate that writes no record");
    assert_eq!(paused.get(), 1, "it paused once, while the hold lasted");
    assert_eq!(holders(&gates.gate).running.len(), 1, "and is on record");
    drop(reader);

    paused.set(0);
    let whole = try_exclusive(open(&gates.gate).unwrap()).unwrap();
    assert!(
        matches!(
            try_shared(open(&gates.gate).unwrap()),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "a running holder of the whole gate on record refuses a reader"
    );
    assert_eq!(paused.get(), 0, "at once, with no pause");
    drop(whole);

    let kept = crate::lock::Flock::try_lock(open(&gates.gate).unwrap()).unwrap();
    let began = std::time::Instant::now();
    assert!(
        matches!(
            try_shared(open(&gates.gate).unwrap()),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "a hold that writes no record and outlasts the bound refuses the reader"
    );
    assert!(
        began.elapsed() >= SETTLE,
        "only once the bound passed: {:?}",
        began.elapsed()
    );
    assert!(paused.get() >= 1, "having paused while it waited");
    drop(kept);

    // A line of a holder of the whole gate that has ended (this pid, another birth) is no running
    // holder: the reader rides out a hold that writes no record beside it.
    let me = i32::try_from(std::process::id()).unwrap();
    let birth = aterm_uds::process::read_birth(me).unwrap().0;
    std::fs::write(
        &gates.gate,
        format!(
            "holder pid={me} birth={} mode=exclusive command=gone\n",
            birth + 1
        ),
    )
    .unwrap();
    assert!(holders(&gates.gate).running.is_empty());
    paused.set(0);
    *bare.borrow_mut() = Some(crate::lock::Flock::try_lock(open(&gates.gate).unwrap()).unwrap());
    let reader = try_shared(open(&gates.gate).unwrap())
        .expect("an ended holder of the whole gate on record refuses no reader");
    assert_eq!(paused.get(), 1, "it paused once, while the hold lasted");
    drop(reader);
}

/// A line whose pid runs now with another birth than the one it recorded is a holder that has
/// ended, a pid given again: it is never named as the holder.
#[test]
fn a_line_whose_pid_runs_with_another_birth_is_a_holder_that_has_ended() {
    let gates = Gates::new();
    let me = i32::try_from(std::process::id()).unwrap();
    let birth = aterm_uds::process::read_birth(me).unwrap().0;
    // This process holds the gate as an atpkg from before the record does, beside a line a
    // holder left whose pid is this process's now.
    let raw = crate::lock::Flock::try_lock(crate::metadata_io::open_regular(&gates.gate).unwrap())
        .unwrap();
    std::fs::write(
        &gates.gate,
        format!(
            "holder pid={me} birth={} mode=exclusive command=gone\n",
            birth + 1
        ),
    )
    .unwrap();
    let found = holders(&gates.gate);
    assert!(found.running.is_empty(), "{found:?}");
    assert_eq!(
        found
            .ended
            .iter()
            .map(|holder| holder.pid)
            .collect::<Vec<_>>(),
        [me]
    );
    let words = gates.refused(Duration::ZERO).to_string();
    assert!(
        !words.contains("held by")
            && !words.contains("this process")
            && words.contains(&format!("pid {me} (gone)")),
        "{words}"
    );
    drop(raw);
}

/// A record names each holder of a well-formed, finished line once, as running or ended by the
/// process table; spaces a holder left as it let go, and a line still being written, name none.
#[test]
fn a_record_reads_each_finished_line_once_as_running_or_ended() {
    let bytes = b"holder pid=11 birth=5 mode=shared command=a b\n\
        holder pid=12 birth=6 mode=shared command=c\n\
        \x20\x20\x20\x20\x20\x20\x20\n\
        holder pid=11 birth=5 mode=shared command=a b\n\
        \x20\x20holder pid=13 birth=7 mode=exclusive command=d\n\
        holder pid=14 birth=x mode=shared command=e\n\
        holder pid=15 birth=8 mode=shared command=unfinished";
    let found = checked(bytes, &|holder| holder.pid != 13);
    let pids = |holders: &[Holder]| holders.iter().map(|holder| holder.pid).collect::<Vec<_>>();
    assert_eq!(pids(&found.running), [11, 12]);
    assert_eq!(pids(&found.ended), [13]);
    assert!(found.running.iter().all(|holder| holder.shared));
    let words = found.words();
    let (first, second) = (words.find("pid 11 (a b)"), words.find("pid 12 (c)"));
    assert!(
        first.is_some() && second.is_some() && first < second && !words.contains("pid 13"),
        "each running holder by pid and command, in order, and no other: {words}"
    );
    let ended = Holders {
        running: Vec::new(),
        ended: found.ended.clone(),
    };
    assert!(!ended.awaited());
    let words = ended.words();
    assert!(
        !words.contains("held by") && words.contains("pid 13 (d)"),
        "{words}"
    );
    assert!(
        Holders::default()
            .words()
            .contains("no holder is on record")
    );
}

/// The record adds no entry to the gates folder: holders reading beside each other add a line
/// each and take back exactly their own as they let go, and a holder of the whole gate leaves it
/// empty.
#[test]
fn the_record_lives_in_the_gate_and_each_holder_takes_its_own_back() {
    let gates = Gates::new();
    let entries = || {
        std::fs::read_dir(gates.index.join("locks.noindex"))
            .unwrap()
            .count()
    };
    let before = entries();
    let first = try_shared(open(&gates.gate).unwrap()).unwrap();
    let second = try_shared(open(&gates.gate).unwrap()).unwrap();
    let both = holders(&gates.gate);
    assert_eq!(both.running.len(), 1, "one process, one holder: {both:?}");
    let text = std::fs::read_to_string(&gates.gate).unwrap();
    assert_eq!(text.lines().count(), 2, "{text:?}");
    drop(first);
    let text = std::fs::read_to_string(&gates.gate).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert!(
        lines[0].trim().is_empty() && lines[1].starts_with("holder pid="),
        "{text:?}"
    );
    assert_eq!(holders(&gates.gate).running.len(), 1);
    drop(second);
    assert!(holders(&gates.gate).running.is_empty());
    assert!(
        std::fs::read_to_string(&gates.gate)
            .unwrap()
            .bytes()
            .all(|byte| byte == b' ' || byte == b'\n')
    );
    drop(try_exclusive(open(&gates.gate).unwrap()).unwrap());
    assert_eq!(std::fs::read(&gates.gate).unwrap(), b"");
    assert_eq!(
        entries(),
        before,
        "the record adds no entry to the gates folder"
    );
}

/// A Linux holder's work is its `stat` line's `utime + stime + cutime + cstime`, read after the
/// command name's last `)`: a name holding `) S` and digits does not shift the fields, and a
/// line cut short or with a field that is not a count reads as nothing.
#[test]
fn a_linux_stat_line_gives_its_four_cpu_fields_after_the_names_last_paren() {
    // pid (comm) state ppid pgrp session tty_nr tpgid flags minflt cminflt majflt cmajflt
    // utime stime cutime cstime priority nice ...
    let plain = "4242 (trustc) R 1 4242 4242 0 -1 4194304 900 10 2 0 700 300 20 10 20 0 1 0 5555 0";
    assert_eq!(stat_work(plain), Some(1030));
    let tricky = "77 (a) S 9 9 9 b) R 1 77 77 0 -1 0 0 0 0 0 7 3 2 1 20 0 1 0 1 0";
    assert_eq!(
        stat_work(tricky),
        Some(13),
        "the fields follow the name's last `)`"
    );
    assert_eq!(stat_work("77 (short) R 1 77 77 0 -1 0 0 0 0 0 7 3"), None);
    assert_eq!(
        stat_work("77 (x) R 1 77 77 0 -1 0 0 0 0 0 7 3 two 1 20"),
        None
    );
    assert_eq!(stat_work("no name here"), None);
}
