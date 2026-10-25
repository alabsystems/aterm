// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-learn-bench`, Aos M6's session runner (design §13.9): it checks G0, spawns child A and
//! then child B of `aos-learn session` from a copied lab, each watched at a 3,600 s deadline,
//! captures their output to files, decides G0 to G5 once both have exited, and writes the pair's
//! `void` or `record` line, the record and an `aos-learn-runner-v1` sidecar.
//!
//! It labels nothing and mints nothing: its ledger is `Ledger::runner`, which writes only `void`
//! and `record`. It shows nothing until both children have exited and every gate is decided.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_aos::Digest;
use aterm_durable::{durable, durable_dir};
use aterm_effects::aos_momentum::m4::held_out_judged;
use aterm_effects::aos_momentum::m6::{
    DurableLog, Ledger, Line, M6_CHAIN_V1, Pair, STORE_DOMAIN, Store, VoidCause, VoidClass,
    ledger_head,
};
use aterm_uds::spawnfd::Stdio;

use crate::aos_gpu_bench::{Watch, clean_commit, watch};
use crate::aos_learn::{LabLog, LabStore, Summary, guarded, make_dirs};
use crate::instance::take_value;
use crate::lab_build::{LAB_BUILD, check_build};
use crate::paths::{from_caller, paths};
use crate::stop::{check, install, ring, sleep};
use crate::sys::{await_exit, digest_file};
use crate::trail_gallery::{checkouts, resolve_out};
use crate::{Fail, Res};

/// The verb's own help (`aos-learn-bench --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-learn-bench --lab PATH --out DIR (--pair I | --practice J) --tests-log FILE

Aos M6's session runner, the design's pre-registered session (§13, Measurement). It runs child A
(`aos-learn session --threads 8`) and then child B (`--replay A`, single-threaded) from the copied
lab PATH, each watched at a 3,600 s deadline,
captures their output under DIR, decides the gates G0 to G5, and writes the pair's `void` or
`record` line, DIR/record.txt and DIR/runner.txt. FILE is the tests log the operator ran on the
session commit; its digest goes to both children's `open` lines.
exit: 0 a session of record; 1 refused, stopped, void or a defect; 2 usage
";

/// The first line of the sidecar.
pub const SIDECAR: &str = "aos-learn-runner-v1";

/// The domain of the copied lab's digest (G0).
pub const LAB_DOMAIN: &[u8] = b"aterm-aos-m6-lab-v1\0";

/// The domain of the tests log's digest.
pub const TESTS_LOG_DOMAIN: &[u8] = b"aterm-aos-m6-tests-log-v1\0";

/// Each child's deadline.
pub const DEADLINE: Duration = Duration::from_secs(3_600);

/// The most physical footprint a child may reach (G4).
pub const MAX_FOOTPRINT: u64 = 2 << 30;

/// The most bytes of a tests log the runner reads.
const MAX_TESTS_LOG: u64 = 64 << 20;

/// The most bytes of a store's `store.txt` the runner reads.
const MAX_STORE_TEXT: u64 = 4 << 10;

/// What the runner was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Args {
    lab: String,
    out: String,
    pair: Pair,
    tests_log: String,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let mut rest = args.to_vec();
    let need = |v: Option<String>, flag: &str| {
        v.ok_or_else(|| Fail::usage(format!("aos-learn-bench: {flag} is required")))
    };
    let lab = need(take_value(&mut rest, "--lab")?, "--lab PATH")?;
    let out = need(take_value(&mut rest, "--out")?, "--out DIR")?;
    let tests_log = need(take_value(&mut rest, "--tests-log")?, "--tests-log FILE")?;
    let real = take_value(&mut rest, "--pair")?;
    let practice = take_value(&mut rest, "--practice")?;
    if let Some(extra) = rest.first() {
        return Err(Fail::usage(format!(
            "aos-learn-bench: unexpected '{extra}'"
        )));
    }
    let number = |flag: &str, v: &str| {
        v.parse::<u32>()
            .ok()
            .filter(|n| v == n.to_string())
            .ok_or_else(|| Fail::usage(format!("aos-learn-bench: {flag} {v}: not a number")))
    };
    let pair = match (real, practice) {
        (Some(i), None) => match number("--pair", &i)? {
            i @ 1..=3 => Pair::Real(i),
            i => {
                return Err(Fail::usage(format!(
                    "aos-learn-bench: --pair {i}: the real pairs are 1, 2 and 3"
                )));
            }
        },
        (None, Some(j)) => Pair::Practice(number("--practice", &j)?),
        _ => {
            return Err(Fail::usage(
                "aos-learn-bench: name one of --pair I and --practice J",
            ));
        }
    };
    Ok(Args {
        lab,
        out,
        pair,
        tests_log,
    })
}

/// One child's run, as the runner saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Child {
    /// `A` or `B`.
    pub name: String,
    /// Whether it was spawned.
    pub spawned: bool,
    /// Its exit code, when it exited.
    pub exit: Option<i32>,
    /// The signal that ended it, if one did.
    pub signal: Option<i32>,
    /// Whether the watcher killed it at the deadline.
    pub deadline: bool,
    /// Whether the watcher killed it on the runner's interrupt.
    pub interrupted: bool,
    /// User plus system CPU, in ms.
    pub cpu_ms: Option<u128>,
    /// Its resident peak, in bytes.
    pub maxrss: Option<u64>,
    /// Its wall time, MEASURED.
    pub wall_ms: u128,
    /// Its summary, parsed from its stdout.
    pub summary: Option<Summary>,
    /// What went wrong around it: a spawn or wait failure.
    pub error: Option<String>,
}

impl Child {
    /// Whether it ran to a clean finish: exit 0 and a summary whose result is `ok`.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.exit == Some(0)
            && self.signal.is_none()
            && !self.deadline
            && !self.interrupted
            && self.error.is_none()
            && self.summary.as_ref().is_some_and(|s| s.result == "ok")
    }

    /// The void cause its own end names, if it did not finish cleanly (design §13.4, rule 6).
    #[must_use]
    pub fn cause(&self) -> Option<VoidCause> {
        if !self.spawned || self.ok() {
            None
        } else if self.deadline {
            Some(VoidCause::Deadline)
        } else if self.interrupted || self.signal.is_some() || self.error.is_some() {
            Some(VoidCause::Other)
        } else {
            Some(VoidCause::ChildFailed)
        }
    }
}

/// What the gates read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inputs {
    /// The pair.
    pub pair: Pair,
    /// The runner's build and profile.
    pub build: String,
    /// The runner's profile.
    pub profile: String,
    /// The copied lab's digest at the start and at the end.
    pub lab: (Digest, Option<Digest>),
    /// The tests log's digest.
    pub tests_log: Digest,
    /// Child A and child B.
    pub children: [Child; 2],
    /// The pair's lines this session wrote: `None` when the runner could not read the ledger.
    pub session: Option<Vec<Line>>,
    /// Whether an interrupt arrived before the runner's last check, made after every other input
    /// is read and just before it decides its line, during the children or after them: an opened
    /// session is then void, whatever its gates: `Other`, unless a child's own end names the
    /// cause ([`Child::cause`], which comes first).
    pub stopped: bool,
    /// G2's store comparison: `None` when B did not finish, else the differences found.
    pub stores: Option<Vec<String>>,
}

/// The gates' decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gates {
    /// Each gate's failures, by name.
    pub failed: Vec<(&'static str, String)>,
    /// Whether every gate was decided: both children finished and the ledger was read, so G2 ran
    /// and G3 and G4 read both.
    pub decided: bool,
    /// The line the runner writes: `None` for none.
    pub line: Option<Verdict>,
}

/// What the runner wrote to the pair's ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wrote {
    /// The `record` line.
    Record,
    /// A `void` line, by its cause and the class m6 gave it.
    Void(VoidCause, VoidClass),
    /// No line, and why.
    Nothing(String),
}

impl Wrote {
    /// As the record and the sidecar name it.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Wrote::Record => "record".to_owned(),
            Wrote::Void(cause, class) => format!("void {cause:?} {}", class_name(*class)),
            Wrote::Nothing(why) => format!("none: {why}"),
        }
    }
}

/// Whether the pair's new lines `session` are this session's: none, or a first line that is child
/// A's own `open` (not a replay) on the store `store.txt` names, which its summary, when it left
/// one, names too. A child killed before it printed its summary is still known by its store.
#[must_use]
pub fn ours(session: &[Line], store: Option<Digest>, named: Option<Digest>) -> bool {
    match session.first() {
        None => true,
        Some(Line::Open {
            store: s,
            replay: false,
            ..
        }) => store == Some(*s) && named.is_none_or(|n| n == *s),
        Some(_) => false,
    }
}

/// One pair directory's ledger head, by the directory's name, as the record prints it (design
/// §13.4, rule 4): its digest, or why its lines do not chain.
pub type Head = (String, Result<Digest, String>);

/// `head` as `<name> head <hex>`, or `<name> BROKEN: <why>`.
fn head_text((name, head): &Head) -> String {
    match head {
        Ok(d) => format!("{name} head {}", d.hex()),
        Err(why) => format!("{name} BROKEN: {why}"),
    }
}

/// Every pair directory's ledger head under `root`, each checked link by link, read only, each
/// fault on its own directory's line; a root that cannot be read is one line, `root`.
fn heads(root: &Path) -> Vec<Head> {
    match LabLog::pairs(root) {
        Ok(all) => all
            .into_iter()
            .map(|(name, read)| {
                let head = read
                    .map_err(|e| format!("{e:?}"))
                    .and_then(|(pair, raw)| ledger_head(pair, &raw).map_err(|e| e.to_string()));
                (name, head)
            })
            .collect(),
        Err(e) => vec![("root".to_owned(), Err(format!("{e:?}")))],
    }
}

/// The new file `path`, for writing: one that exists already is refused, never truncated.
fn new_file(path: &Path) -> std::io::Result<File> {
    File::options().write(true).create_new(true).open(path)
}

/// Writes `text` to the new file `path` and flushes it durably.
fn save(path: &Path, text: &str) -> std::io::Result<()> {
    let mut f = new_file(path)?;
    f.write_all(text.as_bytes())?;
    durable(&f)
}

/// Child `name`'s new capture files under `out`, `<name>.stdout` and `<name>.stderr`.
fn capture(out: &Path, name: &str) -> std::io::Result<(File, File)> {
    let stdout = new_file(&out.join(format!("{name}.stdout")))?;
    Ok((stdout, new_file(&out.join(format!("{name}.stderr")))?))
}

/// The id of the store under `dir`, from its `store.txt`, which `Store::init` makes first and
/// whole (by link) before the session's `open` line; `None` when it holds none.
fn store_id(dir: &Path) -> Option<Digest> {
    let mut bytes = Vec::new();
    File::open(dir.join("store.txt"))
        .and_then(|f| f.take(MAX_STORE_TEXT + 1).read_to_end(&mut bytes))
        .ok()?;
    (bytes.len() as u64 <= MAX_STORE_TEXT).then(|| Digest::of(STORE_DOMAIN, &[&bytes]))
}

/// The line a session ends with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A finished session of record.
    Record,
    /// A void, by its cause.
    Void(VoidCause),
}

/// The bits the session's `look-verdict` lines recorded.
fn recorded_bits(session: &[Line]) -> String {
    session
        .iter()
        .filter_map(|l| match l {
            Line::LookVerdict { bit } => Some(if *bit { '1' } else { '0' }),
            _ => None,
        })
        .collect()
}

/// The session's `open` line for the store `store`, flagged `replay` or not.
fn open_of(session: &[Line], store: Option<Digest>, replay: bool) -> Option<&Line> {
    session.iter().find(|l| {
        matches!(l, Line::Open { store: s, replay: r, .. } if Some(*s) == store && *r == replay)
    })
}

/// G0 to G5 over `i` (design §13.9). A session whose lines hold no non-replay `open` (child A's
/// own) was stopped before it:
/// it gets no line.
#[must_use]
pub fn decide(i: &Inputs) -> Gates {
    let mut g = Gates::default();
    let mut fail = |gate: &'static str, why: String| g.failed.push((gate, why));
    let [a, b] = &i.children;
    // G0: one clean commit, the stamps equal, the lab unchanged, the profile declared.
    let commit = i.build.rsplit_once("+g").map_or("", |(_, c)| c);
    if !clean_commit(commit) {
        fail(
            "G0",
            format!("the runner's build {} names no clean commit", i.build),
        );
    }
    for c in [a, b].into_iter().filter(|c| c.spawned) {
        if let Some(s) = &c.summary {
            if s.build != i.build {
                fail(
                    "G0",
                    format!("child {} ran {}, not {}", c.name, s.build, i.build),
                );
            }
            if s.profile != i.profile {
                fail(
                    "G0",
                    format!(
                        "child {} was built {}, not {}",
                        c.name, s.profile, i.profile
                    ),
                );
            }
        }
    }
    if i.lab.1 != Some(i.lab.0) {
        fail("G0", "the copied lab changed during the session".to_owned());
    }
    // G3: the guard read 0, and both open lines hold the tests log's digest.
    for c in [a, b].into_iter().filter(|c| c.spawned) {
        match &c.summary {
            Some(s) if s.held_out_judged == 0 => {}
            Some(s) => fail(
                "G3",
                format!(
                    "child {} read held_out_judged {}",
                    c.name, s.held_out_judged
                ),
            ),
            // A child with no summary was killed or failed before it printed one: its own end
            // names its void (`Child::cause`), and its guard was not read, as the record says.
            None => {}
        }
    }
    let store_of = |c: &Child| c.summary.as_ref().and_then(|s| s.store);
    let session = i.session.as_deref();
    for (c, replay) in [(a, false), (b, true)] {
        let Some(session) = session.filter(|_| c.ok()) else {
            continue;
        };
        match open_of(session, store_of(c), replay) {
            Some(Line::Open { tests_log, .. }) if *tests_log == i.tests_log => {}
            Some(_) => fail(
                "G3",
                format!("child {}'s open line holds another tests log", c.name),
            ),
            None => fail(
                "G3",
                format!("child {} opened no line on its store", c.name),
            ),
        }
    }
    // G4: no deadline kill, and each clean child's footprint at most 2 GiB.
    for c in [a, b] {
        if c.deadline {
            fail("G4", format!("child {} was killed at the deadline", c.name));
        }
        if c.ok() {
            match c.summary.as_ref().and_then(|s| s.footprint) {
                Some(f) if f <= MAX_FOOTPRINT => {}
                Some(f) => fail(
                    "G4",
                    format!("child {}'s footprint {f} passes 2 GiB", c.name),
                ),
                None => fail("G4", format!("child {} reported no footprint", c.name)),
            }
        }
    }
    // G2: B equals A, store and bits, once both finished; the bits only when the ledger was read.
    let finished = a.ok() && b.ok();
    g.decided = finished && session.is_some();
    if let Some(session) = session.filter(|_| finished) {
        let recorded = recorded_bits(session);
        for c in [a, b] {
            let bits = c.summary.as_ref().map_or("", |s| s.bits.as_str());
            if bits != recorded {
                fail(
                    "G2",
                    format!(
                        "child {}'s bits {bits} are not the ledger's {recorded}",
                        c.name
                    ),
                );
            }
        }
    }
    if finished {
        match &i.stores {
            Some(diffs) => {
                for d in diffs {
                    fail("G2", d.clone());
                }
            }
            None => fail("G2", "the stores were not compared".to_owned()),
        }
    }
    let opened = session.is_some_and(|s| {
        s.iter()
            .any(|l| matches!(l, Line::Open { replay: false, .. }))
    });
    g.line = if !opened {
        None
    } else if let Some(cause) = a.cause().or_else(|| b.cause()) {
        Some(Verdict::Void(cause))
    } else if !b.spawned || i.stopped {
        Some(Verdict::Void(VoidCause::Other))
    } else if g.failed.is_empty() {
        Some(Verdict::Record)
    } else {
        Some(Verdict::Void(VoidCause::Gate))
    };
    g
}

/// The files under `dir/sub`, by their paths below `dir`, with their bytes.
fn files_under(dir: &Path, sub: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
        let entries = match fs::read_dir(at) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("cannot read {}: {e}", at.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read {}: {e}", at.display()))?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
            if kind.is_dir() {
                walk(root, &path, out)?;
            } else {
                let bytes =
                    fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                let name = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
                out.push((name.into_owned(), bytes));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, &dir.join(sub), &mut out)?;
    out.sort();
    Ok(out)
}

/// G2's store comparison of A's and B's stores (design §13.9): every version and eval file byte
/// for byte, and the revisions' (reason, active, head) in order. Store ids and the ledger heads
/// the revisions record are excluded.
/// # Errors
/// A store that cannot be read or loaded.
pub fn compare_stores(a: &Path, b: &Path, build: &str) -> Result<Vec<String>, String> {
    let mut diffs = Vec::new();
    for sub in ["versions", "eval"] {
        let (fa, fb) = (files_under(a, sub)?, files_under(b, sub)?);
        if fa.is_empty() {
            diffs.push(format!("store A holds no {sub} files"));
        }
        let names = |f: &[(String, Vec<u8>)]| f.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        if names(&fa) != names(&fb) {
            diffs.push(format!("the stores' {sub} files differ in their names"));
        } else {
            for ((name, x), (_, y)) in fa.iter().zip(&fb) {
                if x != y {
                    diffs.push(format!("{name} differs between the stores"));
                }
            }
        }
    }
    let load = |dir: &Path| {
        let io = LabStore::open(dir.to_path_buf())?;
        Store::load(io, build).map_err(|e| format!("{}: {e}", dir.display()))
    };
    let (sa, sb) = (load(a)?, load(b)?);
    let walk = |s: &Store<LabStore>| {
        s.revisions()
            .iter()
            .map(|r| (r.reason, r.active, r.head))
            .collect::<Vec<_>>()
    };
    if walk(&sa) != walk(&sb) {
        diffs.push("the stores' revisions differ in (reason, active, head)".to_owned());
    }
    Ok(diffs)
}

/// The record's text (design §13.9): every gate, both children, the line written and the marks,
/// with "no claim" for anything but a session of record on a real pair whose `record` line the
/// runner wrote.
#[must_use]
pub fn record_text(i: &Inputs, g: &Gates, wrote: &Wrote, heads: &[Head]) -> String {
    let mut out = format!(
        "aos-learn-bench: {} at {} ({})\nlab {} unchanged {}\ntests-log {}\n",
        i.pair.name(),
        i.build,
        i.profile,
        i.lab.0.hex(),
        if i.lab.1 == Some(i.lab.0) {
            "yes"
        } else {
            "NO"
        },
        i.tests_log.hex(),
    );
    for c in &i.children {
        let s = c.summary.as_ref();
        out.push_str(&format!(
            "child {}: {} exit {} signal {} deadline {} cpu_ms {} maxrss {} wall_ms {} MEASURED \
             footprint {} result {}\n",
            c.name,
            if c.spawned { "ran" } else { "not run" },
            c.exit.map_or_else(|| "-".to_owned(), |e| e.to_string()),
            c.signal.map_or_else(|| "-".to_owned(), |e| e.to_string()),
            if c.deadline { "KILLED" } else { "no" },
            c.cpu_ms.map_or_else(|| "-".to_owned(), |e| e.to_string()),
            c.maxrss.map_or_else(|| "-".to_owned(), |e| e.to_string()),
            c.wall_ms,
            s.and_then(|s| s.footprint)
                .map_or_else(|| "-".to_owned(), |e| e.to_string()),
            s.map_or_else(|| "no summary".to_owned(), |s| s.result.clone()),
        ));
        if let Some(e) = &c.error {
            out.push_str(&format!("child {}: {e}\n", c.name));
        }
        if let Some((bytes, versions)) = s.and_then(|s| s.retained) {
            out.push_str(&format!(
                "child {}: retained {bytes} bytes in {versions} versions\n",
                c.name
            ));
        }
    }
    if !i.children.iter().all(Child::ok) {
        out.push_str(
            "gates: G2 not decided, nor G3's open line and G4's footprint for a child that did \
             not finish\n",
        );
    }
    for c in &i.children {
        let why = if !c.spawned {
            "did not run"
        } else if c.summary.is_none() {
            "left no summary"
        } else {
            continue;
        };
        out.push_str(&format!(
            "gates: G3's guard not read for child {}, which {why}\n",
            c.name
        ));
    }
    if i.session.is_none() {
        out.push_str(
            "gates: G2's bits and G3's open lines not decided: the pair's ledger was not read\n",
        );
    }
    if g.decided && g.failed.is_empty() {
        out.push_str("gates: G0 G2 G3 G4 pass\n");
    }
    if i.stopped {
        out.push_str(if matches!(wrote, Wrote::Void(..)) {
            "stopped: an interrupt came before the runner's line; the session is void, by the cause \
             G5 names\n"
        } else {
            "stopped: an interrupt came before the runner's line, and no void was written (G5 \
             says why)\n"
        });
    }
    for (gate, why) in &g.failed {
        out.push_str(&format!("{gate} FAILED: {why}\n"));
    }
    out.push_str(&format!("G5: {}\n", wrote.text()));
    for h in heads {
        out.push_str(&format!("ledger {}\n", head_text(h)));
    }
    let claim = *wrote == Wrote::Record
        && g.line == Some(Verdict::Record)
        && matches!(i.pair, Pair::Real(_));
    let a = i.children[0].summary.as_ref();
    if let Some(a) = a {
        out.push_str(&format!(
            "bits {} mf {} r2 {}\n",
            a.bits,
            a.mf.map_or_else(|| "-".to_owned(), |d| d.hex()),
            a.r2.map_or_else(
                || "not exercised".to_owned(),
                |r| format!("rolled back to v{r}")
            ),
        ));
        for c in &a.criteria {
            out.push_str(&format!("{c}\n"));
        }
        for m in &a.moves {
            out.push_str(&format!("move {m}\n"));
        }
        out.push_str(
            "instant rollback costs keeping every version: no full-copy alternative is built; \
             each transition's wall time is not measured within a session (the train and \
             rollback verbs measure one each)\n",
        );
    }
    let control = a.is_some_and(|a| a.bits.as_bytes().get(1) == Some(&b'1'));
    let retained = a.is_some_and(|a| a.retained.is_some());
    let marks = if !claim {
        "no claim".to_owned()
    } else if control {
        "C5 not shown: look 2 admitted the control, so C1 to C3 are not claimed".to_owned()
    } else if !retained {
        "claimed: C0, C5, and each criterion above marked shown; not C4, whose retained bytes \
         child A did not report"
            .to_owned()
    } else {
        "claimed: C0, C4, C5, and each criterion above marked shown".to_owned()
    };
    out.push_str(&format!("marks: {marks}\n"));
    out
}

/// The sidecar: the record's facts, one `key value` line each.
#[must_use]
pub fn sidecar_text(i: &Inputs, g: &Gates, wrote: &Wrote, heads: &[Head]) -> String {
    let mut out = format!(
        "{SIDECAR}\npair {}\nbuild {}\nprofile {}\nlab {}\nlab_unchanged {}\ntests_log {}\n",
        i.pair.name(),
        i.build,
        i.profile,
        i.lab.0.hex(),
        i.lab.1 == Some(i.lab.0),
        i.tests_log.hex(),
    );
    for c in &i.children {
        out.push_str(&format!(
            "child {} spawned={} exit={} signal={} deadline={} interrupted={} wall_ms={} retained={}\n",
            c.name,
            c.spawned,
            c.exit.map_or_else(|| "-".to_owned(), |e| e.to_string()),
            c.signal.map_or_else(|| "-".to_owned(), |e| e.to_string()),
            c.deadline,
            c.interrupted,
            c.wall_ms,
            c.summary
                .as_ref()
                .and_then(|s| s.retained)
                .map_or_else(|| "-".to_owned(), |(b, v)| format!("{b}/{v}")),
        ));
    }
    out.push_str(&format!("decided {}\nstopped {}\n", g.decided, i.stopped));
    for (gate, why) in &g.failed {
        out.push_str(&format!("failed {gate} {why}\n"));
    }
    for h in heads {
        out.push_str(&format!("ledger {}\n", head_text(h)));
    }
    out.push_str(&format!("line {}\n", wrote.text()));
    out
}

/// `aterm-lab aos-learn-bench ...`.
/// # Errors
/// A usage error (exit 2); a refusal, a stop, a void or a defect (exit 1).
pub fn cmd_aos_learn_bench(args: &[String]) -> Res {
    cmd_to(args, &mut std::io::stdout())
}

/// [`cmd_aos_learn_bench`], its help written to `o`.
fn cmd_to(args: &[String], o: &mut dyn Write) -> Res {
    let out = if args.iter().any(|a| a == "-h" || a == "--help") {
        shown(o, HELP);
        Ok(0)
    } else {
        parse(args).and_then(|a| run(&a))
    };
    guarded("aos-learn-bench", out, held_out_judged())
}

/// A regular file `given` names, canonical.
fn regular_file(given: &str, flag: &str) -> Result<PathBuf, Fail> {
    let path = fs::canonicalize(from_caller(given))
        .map_err(|e| Fail::usage(format!("aos-learn-bench: {flag} {given}: {e}")))?;
    if !path.is_file() {
        return Err(Fail::usage(format!(
            "aos-learn-bench: {flag} {given}: not a regular file"
        )));
    }
    Ok(path)
}

/// The tests log's digest, refused unless the log names `commit`.
fn tests_log_digest(path: &Path, commit: &str) -> Result<Digest, Fail> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|f| f.take(MAX_TESTS_LOG + 1).read_to_end(&mut bytes))
        .map_err(|e| {
            Fail::usage(format!(
                "aos-learn-bench: --tests-log {}: {e}",
                path.display()
            ))
        })?;
    if bytes.len() as u64 > MAX_TESTS_LOG {
        return Err(Fail::usage(format!(
            "aos-learn-bench: --tests-log {} passes {MAX_TESTS_LOG} bytes",
            path.display()
        )));
    }
    if !String::from_utf8_lossy(&bytes).contains(commit) {
        return Err(Fail::die(format!(
            "aos-learn-bench: refused TestsLog: {} does not name the session commit {commit}",
            path.display()
        )));
    }
    Ok(Digest::of(TESTS_LOG_DOMAIN, &[&bytes]))
}

/// The copied lab's own build and profile, from `aos-learn build`, which labels nothing.
fn lab_stamp(lab: &Path) -> Result<(String, String), String> {
    let mut cmd = aterm_uds::spawnfd::command(lab);
    cmd.args(["aos-learn", "build"]).stdin(Stdio::null());
    let out = aterm_uds::deadline::output_within(&mut cmd, Duration::from_secs(60))
        .map_err(|e| format!("{}: {e}", lab.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} aos-learn build exited {}",
            lab.display(),
            out.status
        ));
    }
    parse_stamp(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| format!("{} printed no build and profile", lab.display()))
}

/// `aos-learn build`'s two lines.
fn parse_stamp(text: &str) -> Option<(String, String)> {
    let mut lines = text.lines();
    let build = lines.next()?.strip_prefix("build ")?.to_owned();
    let profile = lines.next()?.strip_prefix("profile ")?.to_owned();
    lines.next().is_none().then_some((build, profile))
}

/// Runs `lab` with `args`, its output to `out/<name>.stdout` and `.stderr`, watched at the
/// deadline; then reads its summary.
fn run_child(lab: &Path, name: &str, args: &[String], out: &Path) -> Child {
    let mut c = Child {
        name: name.to_owned(),
        spawned: true,
        ..Child::default()
    };
    let stdout = out.join(format!("{name}.stdout"));
    let kept = capture(out, name).and_then(|(o, e)| Ok(((o.try_clone()?, e.try_clone()?), (o, e))));
    let (captured, (o, e)) = match kept {
        Ok(f) => f,
        Err(e) => {
            c.spawned = false;
            c.error = Some(format!("cannot make its output files: {e}"));
            return c;
        }
    };
    let mut cmd = aterm_uds::spawnfd::command(lab);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(o))
        .stderr(Stdio::from(e));
    let start = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            c.spawned = false;
            c.error = Some(format!("cannot spawn {}: {e}", lab.display()));
            return c;
        }
    };
    let pid = child.id();
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || watch(pid, DEADLINE, &done))
    };
    let exited = await_exit(pid);
    c.wall_ms = start.elapsed().as_millis();
    done.store(true, Ordering::SeqCst);
    ring();
    match watcher.join().unwrap_or(Watch::Exited) {
        Watch::Exited => {}
        Watch::Deadline => c.deadline = true,
        Watch::Stopped(_) => c.interrupted = true,
    }
    if let Err(e) = exited {
        c.error = Some(format!("waitid: {e}"));
        let _ = child.kill();
    }
    match child.wait_with_rusage() {
        Ok(u) => {
            c.exit = u.status.code();
            c.signal = u.status.signal();
            c.cpu_ms = Some(u.cpu.as_millis());
            c.maxrss = Some(u.maxrss);
        }
        Err(e) => {
            c.error.get_or_insert(format!("wait4: {e}"));
        }
    }
    if let Err(e) = durable(&captured.0).and_then(|()| durable(&captured.1)) {
        c.error
            .get_or_insert(format!("cannot flush its output: {e}"));
    }
    c.summary = fs::read_to_string(&stdout)
        .ok()
        .and_then(|t| Summary::parse(&t));
    c
}

fn run(a: &Args) -> Res {
    install();
    check_build(LAB_BUILD)?;
    let commit = env!("ATERM_LAB_GIT_COMMIT");
    if !clean_commit(commit) {
        return Err(Fail::die(format!(
            "aos-learn-bench: refused G0: the runner's commit {commit} is not clean"
        )));
    }
    let pin = M6_CHAIN_V1.ok_or_else(|| {
        Fail::die("aos-learn-bench: refused ChainMoved: M6_CHAIN_V1 is not pinned (stage E)")
    })?;
    let out = resolve_out(&from_caller(&a.out), &checkouts(&paths().repo))
        .map_err(|e| Fail::usage(format!("aos-learn-bench: --out {e}")))?;
    let empty = match fs::read_dir(&out) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            return Err(Fail::usage(format!(
                "aos-learn-bench: --out {}: {e}",
                out.display()
            )));
        }
    };
    if !empty {
        return Err(Fail::usage(format!(
            "aos-learn-bench: --out {} is not empty: a record never mixes sessions",
            out.display()
        )));
    }
    let lab = regular_file(&a.lab, "--lab")?;
    let lab_digest = digest_file(&lab, LAB_DOMAIN)
        .map_err(|e| Fail::usage(format!("aos-learn-bench: --lab {e}")))?;
    let profile = env!("ATERM_LAB_PROFILE");
    let stamp =
        lab_stamp(&lab).map_err(|e| Fail::die(format!("aos-learn-bench: refused G0: {e}")))?;
    if stamp != (LAB_BUILD.to_owned(), profile.to_owned()) {
        return Err(Fail::die(format!(
            "aos-learn-bench: refused G0: the lab is {} ({}), the runner {LAB_BUILD} ({profile})",
            stamp.0, stamp.1
        )));
    }
    let tests_log = tests_log_digest(&regular_file(&a.tests_log, "--tests-log")?, commit)?;
    let p = paths();
    let home = p
        .need_home()
        .map_err(|e| Fail::die(format!("aos-learn-bench: refused NoHome: {e}")))?;
    if p.scratch_home() {
        return Err(Fail::die(
            "aos-learn-bench: refused ScratchHome: HOME is a scratch home, and a session's ledgers \
             live under the account's own",
        ));
    }
    let root = LabLog::version_root(home);
    let _runner = LabLog::hold_runner(&root)
        .map_err(|e| Fail::die(format!("aos-learn-bench: refused RunnerBusy: {e}")))?;
    let before = LabLog::read_only(&root, a.pair)
        .lines()
        .map_err(|e| Fail::die(format!("aos-learn-bench: refused Log: {e:?}")))?
        .len();
    make_dirs(&out).map_err(|e| {
        Fail::die(format!(
            "aos-learn-bench: cannot make {}: {e}",
            out.display()
        ))
    })?;

    let pair_flag = match a.pair {
        Pair::Real(i) => ["--pair".to_owned(), i.to_string()],
        Pair::Practice(j) => ["--practice".to_owned(), j.to_string()],
    };
    let session = |store: &str| {
        let mut v = vec![
            "aos-learn".to_owned(),
            "session".to_owned(),
            "--store".to_owned(),
            out.join(store).to_string_lossy().into_owned(),
        ];
        v.extend(pair_flag.iter().cloned());
        v.extend(["--tests-log-digest".to_owned(), tests_log.hex()]);
        v
    };
    let mut args_a = session("A");
    args_a.extend(["--threads".to_owned(), "8".to_owned()]);
    let child_a = run_child(&lab, "A", &args_a, &out);
    let child_b = if child_a.ok() {
        let mut args_b = session("B");
        args_b.extend([
            "--replay".to_owned(),
            out.join("A").to_string_lossy().into_owned(),
        ]);
        run_child(&lab, "B", &args_b, &out)
    } else {
        Child {
            name: "B".to_owned(),
            ..Child::default()
        }
    };
    let stores = (child_a.ok() && child_b.ok()).then(|| {
        compare_stores(&out.join("A"), &out.join("B"), LAB_BUILD).unwrap_or_else(|e| vec![e])
    });

    // The ledger, once the children have let it go, waited for by the stop rule's sleep, so an
    // interrupt ends the wait. A ledger that cannot be taken is reported, never skipped: the
    // record then says the pair's lines were not read, nor G2's bits and G3's open lines
    // decided, and no line was written.
    let pause = |d| sleep(d).map_err(|_| "interrupted while waiting for ledger.lock".to_owned());
    let mut ledger = LabLog::open_within(&root, a.pair, DEADLINE, &pause)
        .map_err(|e| format!("LedgerBusy: {e}"))
        .and_then(|log| Ledger::runner(log, a.pair, pin).map_err(|e| format!("Ledger: {e}")));
    let new = ledger
        .as_ref()
        .ok()
        .map(|l| l.lines().get(before..).unwrap_or_default().to_vec());
    let lab_end = digest_file(&lab, LAB_DOMAIN).ok();
    let store_a = store_id(&out.join("A"));
    // An interrupt before the line voids an opened session, after the children as during them.
    // It is read after the runner's last read before the line, so only the gates, computed in
    // memory, lie between it and the line's own write.
    let stopped = child_a.interrupted || child_b.interrupted || check().is_err();
    let inputs = Inputs {
        pair: a.pair,
        build: LAB_BUILD.to_owned(),
        profile: profile.to_owned(),
        lab: (lab_digest, lab_end),
        tests_log,
        children: [child_a, child_b],
        session: new,
        stopped,
        stores,
    };
    let gates = decide(&inputs);
    let named = inputs.children[0].summary.as_ref().and_then(|s| s.store);
    let wrote = match &mut ledger {
        Err(e) => Wrote::Nothing(format!(
            "the ledger could not be taken ({e}), so its lines were not read and no line was written"
        )),
        Ok(_)
            if !ours(
                inputs.session.as_deref().unwrap_or_default(),
                store_a,
                named,
            ) =>
        {
            Wrote::Nothing(
                "the pair's new lines are not this session's, so the runner writes nothing"
                    .to_owned(),
            )
        }
        Ok(l) => match gates.line {
            None => Wrote::Nothing("the session stopped before its open".to_owned()),
            Some(Verdict::Record) => match l.record() {
                Ok(()) => Wrote::Record,
                Err(e) => Wrote::Nothing(format!("the record line was refused: {e}")),
            },
            Some(Verdict::Void(cause)) => match l.void(cause) {
                Ok(class) => Wrote::Void(cause, class),
                Err(e) => Wrote::Nothing(format!("the void line was refused: {e}")),
            },
        },
    };
    drop(ledger);
    let heads = heads(&root);
    let record = record_text(&inputs, &gates, &wrote, &heads);
    let saved = save(&out.join("record.txt"), &record)
        .and_then(|()| {
            save(
                &out.join("runner.txt"),
                &sidecar_text(&inputs, &gates, &wrote, &heads),
            )
        })
        .and_then(|()| durable_dir(&out));
    shown(&mut std::io::stdout(), &record);
    saved.map_err(|e| Fail::die(format!("aos-learn-bench: cannot write the record: {e}")))?;
    if stopped {
        return Err(Fail::die(format!(
            "aos-learn-bench: interrupted; G5: {}",
            wrote.text()
        )));
    }
    Ok(if wrote == Wrote::Record { 0 } else { 1 })
}

/// Writes `text` to `o` (stdout), ignoring a stdout that is gone (a pipe whose reader an interrupt
/// killed, or a hung-up tty): the record is in its saved files, and the exit must still pass the
/// guard rather than panic.
fn shown(o: &mut dyn Write, text: &str) {
    let _ = o.write_all(text.as_bytes()).and_then(|()| o.flush());
}

/// A void class as the record names it.
const fn class_name(class: VoidClass) -> &'static str {
    match class {
        VoidClass::Stopped => "(stopped: the pair may be opened again)",
        VoidClass::Opening => "(opening: the next pair may be opened)",
        VoidClass::Closing => "(closing: the policy version is closed)",
    }
}

#[cfg(test)]
#[path = "aos_learn_bench_tests.rs"]
mod tests;
