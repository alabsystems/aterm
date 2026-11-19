// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W-kin at a test binary's end (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.1): the kin
//! verify booked of a test binary ([`crate::exec::StageGroupEnd::kin_seen`]) kept in the run's
//! sidecar (`<id>.kin`, [`aterm_uds::runs::kin_path`]), what the binary's end adds to them
//! ([`KinLedger::left`]), and the run's ender, which ends each one W-kin proves, by pid, on a
//! thread of its own so the stage that left them goes on at once.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Instant;

use aterm_uds::runs::{
    KIN_LINES_MAX, Kin, KinJudge, LINK, kin_at_end, kin_header_text, kin_line, open_regular_write,
    write_line, write_record_text,
};

use super::{
    Claims, Fate, Judge, Kernel, Left, LiveKernel, Poster, RunId, Told, Verdict, exe_word,
    launcher_alive, read_records, settle,
};
use crate::findings::tally_words;

/// What [`KinLedger::left`] cost over a run, with what the run booked and its ender ended: what
/// the run-end report's `kin-left` event says. Its reads and times are the closure's at each
/// test binary's end ([`kin_at_end`]) and its sidecar writes alone. They leave out the binds at
/// each tree read while a binary runs (`exec_kin.rs`), the process-table sweep each of
/// `exec::finish`'s kill paths begins before its group signal (one sweep, which on macOS waits
/// up to `aterm_sysprobe`'s `TABLE_WAIT`, 1 s, for a held process table), and the ender's
/// judgments and signals (each round up to two `END_GRACE`s per 16 ends), none of which it
/// times.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KinCost {
    /// Test binaries whose end it read.
    pub stages: u64,
    /// Kernel reads the closure made ([`kin_at_end`]): a hop per listed process, the lineages,
    /// sessions and group listings of each kin's group, and each program and chain it read.
    pub reads: u64,
    /// The closure's wall time with its sidecar writes, summed, microseconds.
    pub wall_us: u64,
    /// Its longest single call, microseconds.
    pub max_us: u64,
    /// Kin booked in all, at tree reads and at binaries' ends.
    pub booked: u64,
    /// Kin the run's ender ended.
    pub ended: u64,
}

/// One test binary's kin, as its end hands them to the ender.
#[derive(Debug)]
struct Batch {
    label: String,
    kin: BTreeSet<Kin>,
}

/// The run's ender: the thread that ends what its test binaries left, and its queue.
#[derive(Debug)]
struct Ender {
    send: mpsc::Sender<Batch>,
    thread: std::thread::JoinHandle<()>,
}

/// What the ender reads its judgments under: the run's own facts, copied at its start.
#[derive(Debug)]
struct Ends {
    dir: PathBuf,
    boot: String,
    id: RunId,
    uniqueid: u64,
    spared: Vec<PathBuf>,
    tty: Option<u32>,
    poster: Arc<dyn Poster>,
    said: Arc<Mutex<Vec<String>>>,
    ended: Arc<AtomicU64>,
    #[cfg(test)]
    spare: bool,
}

/// The sidecar and every kin booked, under one lock.
#[derive(Debug, Default)]
struct Booked {
    file: Option<std::fs::File>,
    /// The sidecar could not be made: kin are still kept here, for this run's own ends.
    unwritten: bool,
    /// The kin lines written, at most [`KIN_LINES_MAX`].
    lines: usize,
    kin: BTreeSet<Kin>,
}

/// A run's kin: what it booked, its sidecar, its ender and what its ends said.
#[derive(Debug)]
pub(super) struct KinLedger {
    path: PathBuf,
    /// The run's own terminal ([`KinJudge::tty`]).
    tty: Option<u32>,
    /// The directories W-kin never proves for this run ([`KinJudge::spared`]).
    spared: Vec<PathBuf>,
    booked: Mutex<Booked>,
    ender: Mutex<Option<Ender>>,
    said: Arc<Mutex<Vec<String>>>,
    ended: Arc<AtomicU64>,
    cost: Mutex<KinCost>,
    /// A test's seam: the ender judges and ends nothing ([`KinLedger::sparing`]).
    #[cfg(test)]
    spare: std::sync::atomic::AtomicBool,
}

impl KinLedger {
    /// The ledger of a run whose sidecar is `path`, whose own terminal is `tty` and whose
    /// locked toolchain lives under `spared`.
    pub(super) fn new(path: PathBuf, tty: Option<u32>, spared: Vec<PathBuf>) -> Self {
        Self {
            path,
            tty,
            spared,
            booked: Mutex::default(),
            ender: Mutex::default(),
            said: Arc::default(),
            ended: Arc::default(),
            cost: Mutex::default(),
            #[cfg(test)]
            spare: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Where the sidecar lives.
    pub(super) fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Book each of `kin` not booked yet, with its line in the sidecar (made, with its header,
    /// for the first): the record's own discipline, one whole line under the file's lock and
    /// this ledger's. Past [`KIN_LINES_MAX`] lines a kin is booked here alone, for this run's own
    /// ends.
    pub(super) fn book<'k>(
        &self,
        runs: &super::Runs,
        pgid: u32,
        kin: impl IntoIterator<Item = &'k Kin>,
    ) {
        let mut booked = self.booked.lock().unwrap_or_else(PoisonError::into_inner);
        for one in kin {
            if !booked.kin.insert(*one) || booked.lines >= KIN_LINES_MAX {
                continue;
            }
            if booked.file.is_none() && !booked.unwritten {
                let header = kin_header_text(&runs.boot, runs.id, self.tty, &self.spared);
                match open_regular_write(&self.path, false)
                    .and_then(|file| write_record_text(&file, &header).map(|()| file))
                {
                    Ok(file) => booked.file = Some(file),
                    Err(_) => booked.unwritten = true,
                }
            }
            if let Some(file) = &booked.file
                && write_line(file, &kin_line(one, pgid)).is_ok()
            {
                booked.lines += 1;
            }
        }
    }

    /// Every kin booked.
    pub(super) fn all(&self) -> BTreeSet<Kin> {
        self.booked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .kin
            .clone()
    }

    /// The sidecar's handle, where it was made.
    pub(super) fn file(&self) -> Option<std::fs::File> {
        self.booked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .file
            .as_ref()
            .and_then(|file| file.try_clone().ok())
    }

    /// The test binary `root`, which led `pgid`, has left its run with `book` booked of it: add
    /// what its end shows ([`kin_at_end`]), then hand them all to the ender and return. Where
    /// the kernel names a parent by its unique id (Apple) that is each process of this uid whose
    /// chain of parent unique ids reaches the binary or a kin; on Linux no kernel record ties an
    /// orphan to the binary, so a descendant no tree read booked is not found. On both, the
    /// members of each group a kin leads that its session or its chain of parents proves the
    /// kin's. Nothing is added that a spared program living at the end started, itself or
    /// through processes living then, below the binary or up to and including the kin it is
    /// found through; what a spared program left by exiting, or started through a process that
    /// has exited since, may be (`ATERM-KI-1117-9214`).
    pub(super) fn left(
        &self,
        runs: &super::Runs,
        pgid: u32,
        label: &str,
        root: &Kin,
        book: &BTreeSet<Kin>,
    ) {
        let began = Instant::now();
        let mut reads = 0;
        let kin = kin_at_end(
            &LiveKernel,
            *root,
            pgid,
            book,
            &self.spared,
            LINK,
            &mut reads,
        );
        self.book(runs, pgid, &kin);
        let took = began.elapsed();
        {
            let mut cost = self.cost.lock().unwrap_or_else(PoisonError::into_inner);
            let us = u64::try_from(took.as_micros()).unwrap_or(u64::MAX);
            cost.stages += 1;
            cost.reads = cost
                .reads
                .saturating_add(u64::try_from(reads).unwrap_or(u64::MAX));
            cost.wall_us = cost.wall_us.saturating_add(us);
            cost.max_us = cost.max_us.max(us);
        }
        if !kin.is_empty() {
            self.queue(
                runs,
                Batch {
                    label: label.to_owned(),
                    kin,
                },
            );
        }
    }

    /// Hand `batch` to the ender, started with the first; where its thread cannot start, the
    /// batch is ended here.
    fn queue(&self, runs: &super::Runs, batch: Batch) {
        let mut ender = self.ender.lock().unwrap_or_else(PoisonError::into_inner);
        if ender.is_none() {
            let ends = Arc::new(self.ends(runs));
            let (send, take) = mpsc::channel::<Batch>();
            let work = Arc::clone(&ends);
            let started = std::thread::Builder::new()
                .name("verify-kin-ender".to_owned())
                .spawn(move || {
                    while let Ok(batch) = take.recv() {
                        work.end(&batch);
                    }
                });
            match started {
                Ok(thread) => *ender = Some(Ender { send, thread }),
                Err(_) => {
                    drop(ender);
                    ends.end(&batch);
                    return;
                }
            }
        }
        if let Some(running) = ender.as_ref()
            && let Err(mpsc::SendError(batch)) = running.send.send(batch)
        {
            drop(ender);
            self.ends(runs).end(&batch);
        }
    }

    fn ends(&self, runs: &super::Runs) -> Ends {
        Ends {
            dir: runs
                .path
                .parent()
                .map_or_else(PathBuf::new, std::path::Path::to_path_buf),
            boot: runs.boot.clone(),
            id: runs.id,
            uniqueid: runs.uniqueid,
            spared: self.spared.clone(),
            tty: self.tty,
            poster: Arc::clone(&runs.poster),
            said: Arc::clone(&self.said),
            ended: Arc::clone(&self.ended),
            #[cfg(test)]
            spare: self.spare.load(Ordering::SeqCst),
        }
    }

    /// Wait for the ender to end every batch handed to it so far, then let its thread go.
    pub(super) fn drain(&self) {
        let ender = self
            .ender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(Ender { send, thread }) = ender {
            drop(send);
            let _ = thread.join();
        }
    }

    /// The receipt's lines of what the ender ended, one per test binary.
    pub(super) fn said(&self) -> Vec<String> {
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// What [`Self::left`] cost, with what was booked and ended.
    pub(super) fn cost(&self) -> KinCost {
        let mut cost = *self.cost.lock().unwrap_or_else(PoisonError::into_inner);
        cost.booked = u64::try_from(self.all().len()).unwrap_or(u64::MAX);
        cost.ended = self.ended.load(Ordering::SeqCst);
        cost
    }

    /// A test's seam: an ender started after this judges and ends nothing, as verify did before
    /// W-kin, so a negative control sees the kin outlive their binary.
    #[cfg(test)]
    pub(super) fn sparing(&self) {
        self.spare.store(true, Ordering::SeqCst);
    }

    /// The run's own terminal and the directories W-kin never proves for it.
    pub(super) fn judge_parts(&self) -> (Option<u32>, &[PathBuf]) {
        (self.tty, &self.spared)
    }

    /// The directories W-kin never proves, books, or books anything through for this run.
    pub(super) fn spared(&self) -> &[PathBuf] {
        &self.spared
    }
}

impl Ends {
    /// End what W-kin proves of `batch` for this run, while it lives: with no roots and no run id
    /// the judge proves by W-kin alone, which C-tree and W-run never widen here (the run's end
    /// judges every kin again by all three). The live runs' claims are read anew: a records
    /// directory that does not admit ends nothing now. Each one ended is told and counted.
    fn end(&self, batch: &Batch) {
        #[cfg(test)]
        if self.spare {
            return;
        }
        let Ok(records) = read_records(&self.dir, Some(&self.boot)) else {
            return;
        };
        let kernel = LiveKernel;
        let claims = Claims::of(
            records
                .iter()
                .map(|(_, r)| r)
                .filter(|r| r.boot == self.boot && r.id != Some(self.id))
                .filter(|r| launcher_alive(r, &kernel)),
        );
        let judge = Judge {
            kernel: &kernel,
            roots: &[],
            id: None,
            run_uniqueid: self.uniqueid,
            groups: Vec::new(),
            horizon: u64::MAX,
            launchd: kernel.launchd_version(),
            claims: &claims,
            kin: Some(KinJudge {
                booked: &batch.kin,
                spared: &self.spared,
                tty: self.tty,
            }),
        };
        let left = end_kin(&judge, &batch.kin, together);
        let told: Vec<String> = left
            .told
            .iter()
            .map(|t| t.line(&format!("left by stage `{}`'s test binary", batch.label)))
            .collect();
        self.poster.post(&told);
        if !left.ended.is_empty() {
            self.ended.fetch_add(
                u64::try_from(left.ended.len()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
            self.said
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(ended_line(&batch.label, &left.ended));
        }
    }
}

/// `stage <label> left 2 processes outside its group: sh ×2, ended`.
pub(super) fn ended_line(label: &str, ended: &[String]) -> String {
    format!(
        "stage {label} left {} processes outside its group: {}, ended",
        ended.len(),
        tally_words(ended)
    )
}

/// What one end of one kin came to.
pub(super) struct Ending {
    pid: i32,
    executable: Option<PathBuf>,
    fate: Fate,
}

/// End `pid` as `judge` proves it, judged again before each signal.
pub(super) fn end_one<K: Kernel>(judge: &Judge<'_, K>, pid: i32) -> Ending {
    let executable = judge.kernel.exe(pid);
    let (fate, _) = settle(judge.kernel, pid, |token| {
        Some(
            judge
                .kernel
                .end(token, &|id| judge.judge(id.pid) == Verdict::Proven),
        )
    });
    Ending {
        pid,
        executable,
        fate,
    }
}

/// The most ends [`together`] makes at once.
const ENDS_AT_ONCE: usize = 16;

/// One round's ends, [`ENDS_AT_ONCE`] at a time, each on a thread of its own, so a round takes
/// what its slowest end takes (at most two [`aterm_uds::process::END_GRACE`]s: its `SIGTERM`'s
/// grace, then its `SIGKILL`'s) per [`ENDS_AT_ONCE`] ends; an end whose thread does not start is
/// made in turn.
fn together(judge: &Judge<'_, LiveKernel>, pids: &[i32]) -> Vec<Ending> {
    if pids.len() < 2 {
        return pids.iter().map(|&pid| end_one(judge, pid)).collect();
    }
    pids.chunks(ENDS_AT_ONCE)
        .flat_map(|chunk| {
            std::thread::scope(|scope| {
                let ends: Vec<_> = chunk
                    .iter()
                    .map(|&pid| {
                        let thread = std::thread::Builder::new()
                            .name("verify-kin-end".to_owned())
                            .spawn_scoped(scope, move || end_one(judge, pid));
                        (pid, thread)
                    })
                    .collect();
                ends.into_iter()
                    .map(|(pid, thread)| match thread {
                        Ok(thread) => thread
                            .join()
                            .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                        Err(_) => end_one(judge, pid),
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

/// End each of `kin` W-kin proves for `judge`'s run, parents first: each round judges what is
/// left in unique-id order and ends what it proves (`round`, all of a round's together), and
/// another round follows one that ended any, since an end orphans what the ended kin started
/// (A4). A kin that reads as no longer booked (gone, or its pid taken) is dropped; one no round
/// proves is left for the run's end to judge again, and named only there.
pub(super) fn end_kin<K: Kernel>(
    judge: &Judge<'_, K>,
    kin: &BTreeSet<Kin>,
    round: impl Fn(&Judge<'_, K>, &[i32]) -> Vec<Ending>,
) -> Left {
    let mut open: Vec<Kin> = kin.iter().copied().collect();
    open.sort_unstable_by_key(|k| k.uniqueid);
    let mut left = Left::default();
    loop {
        let mut proven = Vec::new();
        let mut rest = Vec::new();
        for one in open {
            match judge.judge(one.pid) {
                Verdict::Proven => proven.push(one.pid),
                Verdict::NotOurs => {}
                Verdict::Named | Verdict::Unjudged => rest.push(one),
            }
        }
        if proven.is_empty() {
            return left;
        }
        for ending in round(judge, &proven) {
            if let Fate::Ended { killed } = ending.fate {
                let word = ending
                    .executable
                    .as_deref()
                    .and_then(exe_word)
                    .unwrap_or_else(|| "?".to_owned());
                left.told.push(Told {
                    pid: ending.pid,
                    word: word.clone(),
                    witness: "kin",
                    killed,
                });
                left.ended.push(word);
            }
        }
        open = rest;
    }
}

/// [`end_kin`]'s round for a kernel a thread cannot share: one end after another.
#[cfg(test)]
pub(super) fn in_turn<K: Kernel>(judge: &Judge<'_, K>, pids: &[i32]) -> Vec<Ending> {
    pids.iter().map(|&pid| end_one(judge, pid)).collect()
}
