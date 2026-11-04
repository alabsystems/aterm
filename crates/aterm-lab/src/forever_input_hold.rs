// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A STARTING PROGRAM THAT LEAVES ITS INPUT UNREAD IS CAPTURED, ONCE PER TAB (ATERM-KI-1101-4).
//!
//! On m8 (2026-11-01) Claude Code 2.1.316 left its terminal's replies unread for 1.9 to 102 s after
//! it started; on Andrews-MBP-5 (2026-11-04) 109 starts left nothing unread past 27 ms
//! (`docs/measured/claude-start-input-hold-2026-11-04.md`). The hold is a main thread that, after
//! its REPL mounts, does not get back to its event loop or read its terminal, and nothing captured
//! the process while one stood. So while [`super::started_composer`] watches a start, the look
//! that finds its input unread [`HOLD_CAPTURE_MS`] or longer captures the program once
//! ([`Holds::due`], [`Holds::take`]), into the run's directory beside its scratch, which the
//! jailed program cannot write and `down` keeps ([`dir_of`]): its process tree with each member's
//! age and the tab's `status`, `lsof` of it, and a 2 s `sample`, which names the call its main
//! thread waits in. The start's line names the files, or why a part could not run
//! ([`Capture::said`]); nothing here ends the run. Only a start the ready gate watches is
//! captured: under `--no-ready-gate` no look watches one, and the run says so ([`Holds::said`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use aterm_sysprobe::kproc::Record;
use aterm_uds::deadline::RunError;
use aterm_uds::spawnfd::Command;

use super::Input;
use crate::Fail;
use crate::door::Door;
use crate::run::Run;

/// How long a starting program's input may stand unread (`status input_wait_ms=`) before the look
/// that reads it captures the program. A start that holds nothing leaves its input unread a few
/// ms (on Andrews-MBP-5, 27 ms the longest of 109 starts, and the 50 B of aterm's replies 21 ms);
/// the shortest hold m8 measured was 1.9 s (`at1-a1`). One second is far past the first, and the
/// 2 s sample it starts still overlaps a hold as short as the second.
pub(crate) const HOLD_CAPTURE_MS: u64 = 1_000;

/// The run's own threshold in ms, in place of [`HOLD_CAPTURE_MS`]. `0` also captures a start
/// that holds nothing, at the look that finds it ready: a normal start's capture, to set beside a
/// hold's.
pub(crate) const THRESHOLD_ENV: &str = "LAB_HOLD_CAPTURE_MS";

/// The folder the captures go into, in the run's directory ([`dir_of`]); `down` keeps it whole.
pub(crate) const DIR: &str = "input-holds";

/// Where `run`'s captures go: `runs/<tag>/input-holds`, beside the scratch, not in it. The jail
/// lets its program write anywhere in the scratch and nowhere else in the run's directory, and
/// `sample` writes its file by path, following links, from outside the jail: in the scratch, a
/// link the program planted at a capture's name, or in the folder's place, would have `sample`
/// overwrite a file of the owner's.
pub(crate) fn dir_of(run: &Run) -> PathBuf {
    run.run.join(DIR)
}

/// How long `sample` samples (s).
const SAMPLE_S: &str = "2";

/// The capture children's one hang detector, never their budget: `sample` takes [`SAMPLE_S`] and
/// `lsof` well under a second, and both run at once ([`children`]), so either still running a
/// minute after they started is stuck, ended and said.
const CHILD_HANG: Duration = Duration::from_secs(60);

/// How long a capture child ended at [`CHILD_HANG`] is given to be reaped after its kill.
const CHILD_REAP: Duration = Duration::from_millis(250);

/// The threshold [`THRESHOLD_ENV`]'s value `set` names: [`HOLD_CAPTURE_MS`] where it is unset.
/// # Errors
/// It names no whole number of ms.
pub(crate) fn threshold_of(set: Option<&str>) -> Result<u64, String> {
    set.map_or(Ok(HOLD_CAPTURE_MS), |v| {
        v.trim()
            .parse()
            .map_err(|_| format!("{THRESHOLD_ENV}={v:?} is no whole number of ms"))
    })
}

/// A run's captures: where they go, when a look takes one, and the tabs that have theirs.
#[derive(Debug)]
pub(crate) struct Holds {
    dir: PathBuf,
    threshold_ms: u64,
    taken: BTreeSet<String>,
}

impl Holds {
    /// `run`'s, its threshold [`THRESHOLD_ENV`]'s where that is set.
    /// # Errors
    /// [`THRESHOLD_ENV`] is set to no whole number of ms (a usage error).
    pub(crate) fn of(run: &Run) -> Result<Self, Fail> {
        let threshold_ms = threshold_of(std::env::var(THRESHOLD_ENV).ok().as_deref())
            .map_err(|why| Fail::usage(format!("forever: {why}")))?;
        Ok(Self::new(dir_of(run), threshold_ms))
    }

    /// Captures into `dir`, a look taking one once input stood unread `threshold_ms`.
    pub(crate) fn new(dir: PathBuf, threshold_ms: u64) -> Self {
        Self {
            dir,
            threshold_ms,
            taken: BTreeSet::new(),
        }
    }

    /// The run's line for its captures: none under `--no-ready-gate` (`gated` false), whose starts
    /// no look watches, so that a hold there goes uncaptured is said, not left to be inferred from
    /// no file; else a threshold [`THRESHOLD_ENV`] moved from [`HOLD_CAPTURE_MS`].
    pub(crate) fn said(&self, gated: bool) -> Option<String> {
        let (ms, dir) = (self.threshold_ms, self.dir.display());
        if !gated {
            return Some(
                "no start is captured: under --no-ready-gate no look watches a start, so a hold \
                 there leaves no tree, lsof or sample"
                    .to_string(),
            );
        }
        match ms {
            HOLD_CAPTURE_MS => None,
            0 => Some(format!(
                "every start is captured once into {dir} ({THRESHOLD_ENV}=0): at its first look \
                 that finds its input unread, else at the look that finds it ready"
            )),
            _ => Some(format!(
                "a start whose input stands unread {ms} ms is captured once into {dir} \
                 ({THRESHOLD_ENV}={ms})"
            )),
        }
    }

    /// Whether this look at tab `sid` takes its capture: none is taken for the tab yet, and its
    /// input (`input`, the look's `status`) stands unread at least the threshold, or, with a
    /// threshold of `0`, the look found the start finished (`ready`).
    pub(crate) fn due(&self, sid: &str, input: &Input, ready: bool) -> bool {
        if self.taken.contains(sid) {
            return false;
        }
        let waited = input.wait_ms.parse::<u64>().ok();
        let held = input.unread() && waited.is_some_and(|ms| ms >= self.threshold_ms);
        held || (self.threshold_ms == 0 && ready)
    }

    /// Starts tab `sid`'s one capture, its files named by `label`, at the look whose `status`
    /// reply was `status` (its input `input`): the program is the leader of the tab's foreground
    /// group, as `who` names it (`fgpgid=`).
    pub(crate) fn take(
        &mut self,
        door: &Door<'_>,
        sid: &str,
        label: &str,
        input: &Input,
        status: &str,
    ) -> Taking {
        let pid = crate::harness_proof_judge::fgpgid(&door.out(&["who"]), sid);
        self.take_of(sid, label, input, status, pid)
    }

    /// [`Holds::take`] of the program `pid` leads, where `who` named one.
    pub(crate) fn take_of(
        &mut self,
        sid: &str,
        label: &str,
        input: &Input,
        status: &str,
        pid: Option<u32>,
    ) -> Taking {
        self.taken.insert(sid.to_string());
        let head = format!(
            "# @{sid} {label}: {}, the tab's foreground group {}\n# status: {}\n",
            input.said(),
            pid.map_or_else(|| "-".to_string(), |p| p.to_string()),
            crate::util::one_line(status, 4000)
        );
        Taking::start(&self.dir, label, input, &head, pid)
    }
}

/// What a capture of a starting tab's program took ([`Holds::take`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Capture {
    /// The input of the look that took it.
    pub(crate) at: Input,
    /// The tab's foreground group, whose leader is the program its shell started, where `who`
    /// named one.
    pub(crate) pid: Option<u32>,
    /// That leader's name as the kernel keeps it, empty where its record did not read.
    pub(crate) program: String,
    /// The folder its files went into.
    pub(crate) dir: PathBuf,
    /// Each part's file, or why it wrote none.
    pub(crate) parts: Vec<Part>,
    /// Why nothing was captured, where nothing was.
    pub(crate) why: Option<String>,
}

/// One part of a [`Capture`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    /// `tree`, `lsof` or `sample`.
    pub(crate) what: &'static str,
    /// The file it wrote, or why it wrote none.
    pub(crate) got: Result<PathBuf, String>,
}

impl Capture {
    /// Its words in the start's line: `captured at <input>, pid <p> (<name>), into <dir>: tree
    /// <file>, lsof <file>, sample <file>`, each part that could not run said in its place (`sample
    /// not taken: <why>`), or `not captured at <input>: <why>`.
    pub(crate) fn said(&self) -> String {
        let at = self.at.said();
        if let Some(why) = &self.why {
            return format!("not captured at {at}: {why}");
        }
        let pid = match (self.pid, self.program.as_str()) {
            (None, _) => "-".to_string(),
            (Some(p), "") => p.to_string(),
            (Some(p), name) => format!("{p} ({name})"),
        };
        let parts: Vec<String> = self
            .parts
            .iter()
            .map(|p| match &p.got {
                Ok(file) => format!(
                    "{} {}",
                    p.what,
                    file.file_name().map_or_else(
                        || file.display().to_string(),
                        |n| n.to_string_lossy().into_owned()
                    )
                ),
                Err(why) => format!("{} not taken: {why}", p.what),
            })
            .collect();
        format!(
            "captured at {at}, pid {pid}, into {}: {}",
            self.dir.display(),
            parts.join(", ")
        )
    }
}

/// A capture under way: its tree written, its `lsof` and `sample` running at once on a thread of
/// their own ([`children`]) so the start's looks go on meanwhile ([`Taking::finish`]).
pub(crate) struct Taking {
    capture: Capture,
    rest: Option<std::thread::JoinHandle<Vec<Part>>>,
}

impl Taking {
    fn start(dir: &Path, label: &str, input: &Input, head: &str, pid: Option<u32>) -> Self {
        let mut capture = Capture {
            at: input.clone(),
            pid,
            program: String::new(),
            dir: dir.to_path_buf(),
            parts: Vec::new(),
            why: None,
        };
        let Some(pid) = pid else {
            capture.why = Some("`who` named no foreground group for the tab".to_string());
            return Self {
                capture,
                rest: None,
            };
        };
        if let Err(e) = crate::sys::scratch_dirs(dir) {
            capture.why = Some(format!("{}: {e}", dir.display()));
            return Self {
                capture,
                rest: None,
            };
        }
        let (program, tree) = tree_part(
            dir,
            label,
            head,
            aterm_sysprobe::kproc::records(),
            (pid, now_us()),
            |p| i32::try_from(p).map_or_else(|_| String::new(), crate::procs::command_line),
        );
        capture.program = program;
        capture.parts.push(tree);
        let (lsof_to, sample_to) = (
            dir.join(format!("{label}-lsof.txt")),
            dir.join(format!("{label}-sample.txt")),
        );
        let rest = std::thread::Builder::new()
            .name("input-hold-capture".to_string())
            .spawn(move || {
                let lsof_runs = aterm_sysprobe::jobs::sockets_readable(pid);
                let by = Instant::now() + CHILD_HANG;
                children(pid, (&lsof_to, &sample_to), lsof_runs, by, |cmds, by| {
                    aterm_uds::deadline::outputs_until(cmds, by, CHILD_REAP)
                })
            });
        match rest {
            Ok(rest) => Self {
                capture,
                rest: Some(rest),
            },
            Err(e) => {
                let why = format!("no thread to run it on: {e}");
                capture.parts.extend(["lsof", "sample"].map(|what| Part {
                    what,
                    got: Err(why.clone()),
                }));
                Self {
                    capture,
                    rest: None,
                }
            }
        }
    }

    /// The capture, once its `lsof` and `sample` have ended (both within one [`CHILD_HANG`]).
    pub(crate) fn finish(self) -> Capture {
        let mut capture = self.capture;
        if let Some(rest) = self.rest {
            match rest.join() {
                Ok(parts) => capture.parts.extend(parts),
                Err(_) => capture.parts.extend(["lsof", "sample"].map(|what| Part {
                    what,
                    got: Err("the capture's thread panicked".to_string()),
                })),
            }
        }
        capture
    }
}

/// The capture's `tree` part, into `<dir>/<label>-tree.txt`, from one read of the kernel's table
/// (`records`; `None` where it did not read), with the leader's name (empty where its record is
/// not in the table): [`tree_text`] of the program `pid` at `now_us`, `args` naming a member's
/// command line. A table that did not read writes no file and says so, never a tree that calls
/// the program gone.
pub(crate) fn tree_part(
    dir: &Path,
    label: &str,
    head: &str,
    records: Option<Vec<Record>>,
    (pid, now_us): (u32, u64),
    args: impl Fn(u32) -> String,
) -> (String, Part) {
    let Some(records) = records else {
        return (
            String::new(),
            Part {
                what: "tree",
                got: Err("the kernel's process table did not read".to_string()),
            },
        );
    };
    let program = records
        .iter()
        .find(|r| r.pid == pid)
        .map(|r| r.comm.clone())
        .unwrap_or_default();
    let tree = dir.join(format!("{label}-tree.txt"));
    let text = tree_text(head, &records, pid, now_us, args);
    let got = crate::sys::put(&tree, 0o644, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map(|()| tree.clone())
        .map_err(|e| e.to_string());
    (program, Part { what: "tree", got })
}

/// The tree file's text: `head`, then a row per member of the program's tree from one read of the
/// kernel's table (`records`), the leader `root` first and its descendants after it by start:
/// `pid ppid pgid age_ms comm args`, each age to `now_us`, `args` naming a member's command line.
pub(crate) fn tree_text(
    head: &str,
    records: &[Record],
    root: u32,
    now_us: u64,
    args: impl Fn(u32) -> String,
) -> String {
    let started = |r: &Record| r.start_s * 1_000_000 + u64::from(r.start_us);
    let table: Vec<(i32, i32)> = records
        .iter()
        .filter_map(|r| Some((i32::try_from(r.pid).ok()?, i32::try_from(r.ppid).ok()?)))
        .collect();
    let kin: BTreeSet<u32> = crate::procs::descendants_in(&table, i32::try_from(root).unwrap_or(0))
        .into_iter()
        .filter_map(|p| u32::try_from(p).ok())
        .collect();
    let mut rows: Vec<&Record> = records.iter().filter(|r| kin.contains(&r.pid)).collect();
    rows.sort_by_key(|r| (started(r), r.pid));
    let mut out = format!("{head}pid ppid pgid age_ms comm args\n");
    match records.iter().find(|r| r.pid == root) {
        Some(r) => rows.insert(0, r),
        None => out.push_str(&format!("{root} - - - (gone) -\n")),
    }
    for r in rows {
        out.push_str(&format!(
            "{} {} {} {} {} {}\n",
            r.pid,
            r.ppid,
            r.pgid,
            now_us.saturating_sub(started(r)) / 1000,
            if r.zombie { "<defunct>" } else { &r.comm },
            args(r.pid)
        ));
    }
    out
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

/// The capture's `lsof` and `sample` of `pid`, its parts in that order, run by `run`
/// ([`aterm_uds::deadline::outputs_until`]) at once, under the one deadline `by`: `sample <pid>
/// 2 -mayDie -file <sample_to>` started first, since its 2 s must overlap the hold, and `lsof -nP
/// -p <pid>` into `lsof_to` beside it (no name looked up, so no lookup leaves the Mac) where its
/// sockets may be read now (`lsof_runs`, [`aterm_sysprobe::jobs::sockets_readable`]). A stuck
/// child therefore holds the capture one hang detector at most, and never delays the sample.
fn children(
    pid: u32,
    (lsof_to, sample_to): (&Path, &Path),
    lsof_runs: bool,
    by: Instant,
    run: impl FnOnce(&mut [&mut Command], Instant) -> Vec<Result<Output, RunError>>,
) -> Vec<Part> {
    let pid_word = pid.to_string();
    let mut sample = aterm_uds::spawnfd::command("/usr/bin/sample");
    sample
        .args([pid_word.as_str(), SAMPLE_S, "-mayDie", "-file"])
        .arg(sample_to);
    let mut lsof = aterm_uds::spawnfd::command("/usr/sbin/lsof");
    lsof.args(["-nP", "-p", pid_word.as_str()]);
    let mut outs = if lsof_runs {
        run(&mut [&mut sample, &mut lsof], by)
    } else {
        run(&mut [&mut sample], by)
    }
    .into_iter();
    let sampled = sample_part(ran(outs.next(), "sample"), sample_to);
    let listed = if lsof_runs {
        lsof_part(ran(outs.next(), "lsof"), lsof_to)
    } else {
        Err("the program was exiting, or a fork not yet past its exec".to_string())
    };
    vec![
        Part {
            what: "lsof",
            got: listed,
        },
        sampled,
    ]
}

/// A capture child's output, or why it gave none (`what` the child).
fn ran(out: Option<Result<Output, RunError>>, what: &str) -> Result<Output, String> {
    match out {
        Some(Ok(out)) => Ok(out),
        Some(Err(e)) => Err(format!("{what} {e}")),
        None => Err(format!("{what} was not run")),
    }
}

/// `lsof`'s part: its listing written to `to`, or why there is none.
fn lsof_part(out: Result<Output, String>, to: &Path) -> Result<PathBuf, String> {
    let out = out?;
    if out.stdout.is_empty() {
        return Err(format!(
            "lsof {} named nothing: {}",
            out.status,
            crate::util::one_line(&String::from_utf8_lossy(&out.stderr), 200)
        ));
    }
    crate::sys::put(to, 0o644, &crate::sys::PutSrc::Bytes(&out.stdout))
        .map(|()| to.to_path_buf())
        .map_err(|e| e.to_string())
}

/// `sample`'s part: where the program's threads sat, its main thread's system call included (its
/// own frames are offsets, Claude Code's binary being stripped), in the file `to` it wrote, a
/// regular file read without following a link; or why there is none.
fn sample_part(out: Result<Output, String>, to: &Path) -> Part {
    let got = out.and_then(|out| {
        if !out.status.success() {
            return Err(format!(
                "sample {}: {}",
                out.status,
                crate::util::one_line(&String::from_utf8_lossy(&out.stderr), 200)
            ));
        }
        std::fs::symlink_metadata(to)
            .is_ok_and(|m| m.file_type().is_file() && m.len() > 0)
            .then(|| to.to_path_buf())
            .ok_or_else(|| "sample wrote no file".to_string())
    });
    Part {
        what: "sample",
        got,
    }
}

#[cfg(test)]
#[path = "forever_input_hold_tests.rs"]
mod tests;
