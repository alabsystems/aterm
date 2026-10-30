// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each stage's memory claim in the capacity ledger (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! VERIFY-MEM): taken before the stage starts and held until it ends, admitted alone when only
//! memory no claim holds keeps it out, asked alone for a stage that can never fit, taken at once
//! under `--admit now`, taken by a timed stage only once its run holds the quiet machine, and one
//! interrupt in two stages' waits ending the whole run as COULD NOT RUN. Every claim is in a
//! scratch ledger on a fixed machine: no test touches this user's own.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use aterm_capacity::{Admit, Claim, Error, Fixed, Ledger, Need, Pace, Reading};
use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::checks::Check;
use crate::ladder::Report;
use crate::memory::{Book, StageKey as _};
use crate::plan::{StageId, StageSpec};
use crate::{Ctx, EnvSnapshot, Scope, Seams, Show};

const GIB: u64 = 1 << 30;

/// What can ever be available on the machine every test here claims on: a 24 GiB Mac with 2.5
/// GiB wired.
const REACH: u64 = 43 * GIB / 2;

/// That machine, 20 GiB of it available.
fn machine() -> Fixed {
    on(20)
}

/// That machine, `available` GiB of it available.
fn on(available: u64) -> Fixed {
    Fixed::new(Reading::of_gib(24, available, 14))
}

/// A run's book in `dir`, on that machine.
fn book(dir: &Path) -> Book {
    Book::at(
        Some(dir.join("stage-memory.json")),
        Some(24 * GIB),
        Some(REACH),
    )
}

/// The selection every run here makes.
fn gui() -> Scope {
    Scope::crate_only("aterm-gui")
}

/// Keep `gib` GiB in `book` as stage `id`'s peak in a run over [`gui`] rooted at `root`
/// ([`crate::memory::StageKey::of_run`]).
fn keep(book: &Book, root: &Path, id: StageId, gib: u64) {
    let spec = crate::plan::declared(&gui())
        .into_iter()
        .find(|s| s.id == id)
        .expect("declared");
    let run = Ctx::new(
        root.to_path_buf(),
        gui(),
        EnvSnapshot::default(),
        root.join("scratch"),
    );
    let key = crate::memory::Key::of_run(id, &run);
    book.keep(&spec, &key, gib * GIB).expect("kept");
}

/// Whether a time line in `log` holds `said` as one of its `; `-separated parts, whole.
fn said_whole(log: &str, said: &str) -> bool {
    log.lines()
        .filter(|l| l.trim_start().starts_with("time"))
        .any(|l| l.split("; ").any(|part| part.trim_end() == said))
}

/// What an in-process run's seams saw: each admission asked for (what, bytes, mode), and each
/// stage's look at the ledger as it ran (its title, and the held claims' what and bytes).
#[derive(Debug, Default)]
struct Seen {
    asked: Vec<(String, u64, Admit)>,
    looked: Vec<(String, Vec<(String, u64)>)>,
}

/// The ledger and the record of one test's in-process runs: each test that runs one has its own,
/// since a seam is a plain `fn` and the tests here run at once.
struct Scratch {
    ledger: OnceLock<PathBuf>,
    seen: Mutex<Seen>,
    /// The GiB available on the 24 GiB machine its claims are admitted on.
    available: u64,
}

impl Scratch {
    const fn new() -> Scratch {
        Scratch::on(20)
    }

    /// One whose claims are admitted on the 24 GiB machine with `available` GiB available.
    const fn on(available: u64) -> Scratch {
        Scratch {
            available,
            ledger: OnceLock::new(),
            seen: Mutex::new(Seen {
                asked: Vec::new(),
                looked: Vec::new(),
            }),
        }
    }

    fn ledger(&self) -> Ledger {
        Ledger::at(self.ledger.get().expect("the test set its ledger"))
    }

    /// Admit `need` in the scratch ledger as the live seam does ([`crate::admit_in`]), noting it.
    fn admit(&self, need: Need, mode: Admit) -> Result<Claim, Error> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .asked
            .push((need.what.clone(), need.mem, mode));
        crate::admit_in(
            &self.ledger(),
            &on(self.available),
            Pace {
                poll: std::time::Duration::from_millis(20),
                ..Pace::default()
            },
            need,
            mode,
            &mut |_| {},
        )
    }

    /// A stage that passes, noting the claims held in the scratch ledger as it runs.
    fn stage(&self, spec: &StageSpec) -> Report {
        let held = self
            .ledger()
            .list()
            .expect("the ledger")
            .into_iter()
            .filter(|r| r.held.is_some())
            .map(|r| (r.need.what, r.need.mem))
            .collect();
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .looked
            .push((spec.title.clone(), held));
        let mut r = Report::new(spec.title.clone());
        r.pass("ran");
        r
    }

    /// Forget what earlier runs saw.
    fn clear(&self) {
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Seen::default();
    }

    /// What the runs since [`Self::clear`] saw.
    fn take(&self) -> Seen {
        std::mem::take(&mut *self.seen.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// A `lint` + `fmt` run over [`gui`] rooted at `root`, its stages and admissions `seams`, its
/// memory measured in `book` when one is given and admitted as `mode` says: its exit code and its
/// log, within the hang detector.
fn run(root: &Path, book: Option<Book>, mode: Admit, seams: Seams) -> (i32, String) {
    let mut ctx = Ctx::new(
        root.to_path_buf(),
        gui(),
        EnvSnapshot::default(),
        root.join("scratch"),
    )
    .with_checks(&[Check::Lint, Check::Fmt])
    .with_quiet(Some(mode));
    if let Some(book) = book {
        ctx = ctx.with_memory(book);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut log = Vec::new();
        let code = crate::run_with(
            &ctx,
            &mut std::io::sink(),
            &mut log,
            &Show::default(),
            seams,
        )
        .expect("the result written");
        let _ = tx.send((code, String::from_utf8_lossy(&log).into_owned()));
    });
    rx.recv_timeout(MUST_HAPPEN)
        .expect("hang detector: the run never ended")
}

static HELD: Scratch = Scratch::new();

fn held_admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    HELD.admit(need, mode)
}

fn held_stage(_: &Ctx, spec: &StageSpec) -> Report {
    HELD.stage(spec)
}

/// A stage's memory is claimed before it starts and held until it ends: the lint (never
/// measured, so the cold figure: half of 24 GiB) asks for 12 GiB in the ledger, sees its own
/// claim held while it runs, and leaves none once the run ends; the formatter, which touches no
/// target directory, claims nothing; its time line says what it claimed and why. NEGATIVE
/// CONTROL: the same run measuring no memory asks for no claim, and its lint sees none.
#[test]
fn a_stage_claims_its_memory_before_it_starts_and_holds_it_until_it_ends() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    HELD.ledger
        .set(dir.path().join("ledger"))
        .expect("set once");
    let seams = Seams {
        stage: held_stage,
        admit_quiet: held_admit,
        admit_memory: held_admit,
    };
    let lint = "tippy lint (-p aterm-gui)";
    let (code, log) = run(dir.path(), Some(book(dir.path())), Admit::Wait, seams);
    let seen = HELD.take();
    assert_eq!(code, crate::exit::PASS, "{log}");
    let what = format!("verify {lint}");
    assert_eq!(seen.asked, [(what.clone(), 12 * GIB, Admit::Wait)], "{log}");
    let looked = |seen: &Seen| {
        seen.looked
            .iter()
            .find(|(title, _)| title == lint)
            .map(|(_, held)| held.clone())
            .expect("the lint ran")
    };
    assert_eq!(looked(&seen), [(what, 12 * GIB)]);
    assert!(
        HELD.ledger().list().expect("the ledger").is_empty(),
        "no claim outlives its stage"
    );
    assert!(
        said_whole(
            &log,
            "memory: claimed 12 GiB cold: no run of it is kept, and it compiles (16 GiB, at \
             most half of physical memory)"
        ),
        "{log}"
    );

    HELD.clear();
    let (code, log) = run(dir.path(), None, Admit::Wait, seams);
    let seen = HELD.take();
    assert_eq!(code, crate::exit::PASS, "{log}");
    assert!(seen.asked.is_empty(), "{seen:?}");
    assert!(looked(&seen).is_empty(), "{seen:?}");
    assert!(!log.contains("memory: claimed"), "{log}");
}

static COVERS: Scratch = Scratch::new();

/// What one stage of [`COVERS`]'s runs saw: its title, whether each claim held as it ran covers
/// its tree's builds, the cover marker its children are handed over the run's ([`memory::cover`])
/// with the claim it names then (its `what`), and the run's own.
type CoverSeen = (
    String,
    Vec<bool>,
    Option<(String, Option<String>)>,
    Option<String>,
);

/// What each stage of [`COVERS`]'s runs saw ([`CoverSeen`]).
static COVERS_SEEN: Mutex<Vec<CoverSeen>> = Mutex::new(Vec::new());

fn covers_admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    COVERS.admit(need, mode)
}

fn covers_stage(ctx: &Ctx, spec: &StageSpec) -> Report {
    let records: Vec<_> = COVERS
        .ledger()
        .list()
        .expect("the ledger")
        .into_iter()
        .filter(|r| r.held.is_some())
        .collect();
    let held = records.iter().map(|r| r.need.covers).collect();
    let stage = crate::memory::cover().map(|m| {
        let named = records
            .iter()
            .find(|r| r.cover_marker() == *m)
            .map(|r| r.need.what.clone());
        (m.to_string(), named)
    });
    let run = ctx
        .child_env_add
        .iter()
        .find(|(k, _)| k == aterm_capacity::COVER_ENV)
        .map(|(_, v)| v.to_string_lossy().into_owned());
    COVERS_SEEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((spec.title.clone(), held, stage, run));
    COVERS.stage(spec)
}

/// A stage's memory claim asks to cover the builds its children start (BUILD-ADMIT): the lint's
/// claim, held as it runs, asked to cover its tree, and the lint's children are handed that
/// claim's own cover marker, so a build its test runs is covered by it alone, never by another
/// stage's (`aterm_capacity::covering` reads both; its coverage tests hold what they cover); the
/// run's other children are handed `NO_COVER`, and a stage that claimed nothing none of its
/// own. NEGATIVE CONTROL: a run measuring no memory hands its children no marker and holds no
/// claim.
#[test]
fn a_stages_memory_claim_covers_the_builds_its_children_start() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    COVERS
        .ledger
        .set(dir.path().join("ledger"))
        .expect("set once");
    let seams = Seams {
        stage: covers_stage,
        admit_quiet: covers_admit,
        admit_memory: covers_admit,
    };
    let (code, log) = run(dir.path(), Some(book(dir.path())), Admit::Wait, seams);
    assert_eq!(code, crate::exit::PASS, "{log}");
    let seen = std::mem::take(&mut *COVERS_SEEN.lock().unwrap_or_else(PoisonError::into_inner));
    let lint = seen
        .iter()
        .find(|(title, ..)| title.starts_with("tippy lint"))
        .expect("the lint ran");
    assert_eq!(lint.1, [true], "{seen:?}");
    let (marker, named) = lint.2.as_ref().expect("the lint's own marker");
    assert_eq!(
        named.as_deref(),
        Some(format!("verify {}", lint.0).as_str()),
        "{seen:?}"
    );
    assert_eq!(
        aterm_capacity::Record::marker_holder(marker),
        Some(std::process::id())
    );
    assert!(
        seen.iter()
            .all(|(_, _, _, run)| run.as_deref() == Some(aterm_capacity::NO_COVER)),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .filter(|(_, held, ..)| held.is_empty())
            .all(|(_, _, stage, _)| stage.is_none()),
        "a stage that claimed nothing has no marker of its own: {seen:?}"
    );
    let (code, log) = run(dir.path(), None, Admit::Wait, seams);
    assert_eq!(code, crate::exit::PASS, "{log}");
    let seen = std::mem::take(&mut *COVERS_SEEN.lock().unwrap_or_else(PoisonError::into_inner));
    assert!(
        seen.iter()
            .all(|(_, held, stage, run)| held.is_empty() && stage.is_none() && run.is_none()),
        "{seen:?}"
    );
}

static NEVER: Scratch = Scratch::new();

fn never_admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    NEVER.admit(need, mode)
}

fn never_stage(_: &Ctx, spec: &StageSpec) -> Report {
    NEVER.stage(spec)
}

/// A stage that claims more than can ever be available here, which the ledger would refuse,
/// claims what it needs and asks to run alone: a lint whose kept peak is 25 GiB claims 30, more
/// than the run's 21.5 GiB, and with no other claim held it is admitted alone and says so in one
/// sentence of its time line. NEGATIVE CONTROL: one whose kept peak is 10 GiB claims its own 12
/// GiB beside the others, and its time line says nothing of running alone.
#[test]
fn a_stage_that_can_never_fit_asks_to_run_alone() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    NEVER
        .ledger
        .set(dir.path().join("ledger"))
        .expect("set once");
    let seams = Seams {
        stage: never_stage,
        admit_quiet: never_admit,
        admit_memory: never_admit,
    };
    for (peak, claimed, said) in [
        (
            25,
            30 * GIB,
            "memory: claimed 30 GiB, the most its last 1 run(s) used (25 GiB) and a fifth more, \
             at least 1 GiB: more than the 21.5 GiB that can ever be available here, so it asks \
             to run alone, admitted alone, as it asked: it needs 30 GiB (20 GiB available) and \
             no other claim held memory, so no other memory claim is admitted while it runs",
        ),
        (
            10,
            12 * GIB,
            "memory: claimed 12 GiB, the most its last 1 run(s) used (10 GiB) and a fifth more, \
             at least 1 GiB",
        ),
    ] {
        let at = dir.path().join(peak.to_string());
        std::fs::create_dir_all(&at).expect("a folder");
        let b = book(&at);
        keep(&b, &at, StageId::Tippy, peak);
        NEVER.clear();
        let (code, log) = run(&at, Some(b), Admit::Wait, seams);
        let seen = NEVER.take();
        assert_eq!(code, crate::exit::PASS, "{log}");
        assert_eq!(
            seen.asked
                .iter()
                .map(|(_, mem, _)| *mem)
                .collect::<Vec<_>>(),
            [claimed],
            "a {peak} GiB peak: {log}"
        );
        assert!(said_whole(&log, said), "a {peak} GiB peak: {log}");
    }
}

static ALONE: Scratch = Scratch::on(8);

fn alone_admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    ALONE.admit(need, mode)
}

fn alone_stage(_: &Ctx, spec: &StageSpec) -> Report {
    ALONE.stage(spec)
}

/// THE BINDING REQUIREMENT, IN ONE RUN: on a 24 GiB Mac with 8 GiB available and no other claim
/// in the ledger, the lint's cold 12 GiB, short only of memory no claim holds (other work on
/// this Mac), is admitted alone at its first look and the run ends within the hang detector, its
/// time line saying so in one sentence. NEGATIVE CONTROL: a ledger that waits for the room no
/// claim holds never admits it, and the run fails the hang detector by name.
#[test]
fn a_stage_short_only_of_memory_no_claim_holds_is_admitted_alone() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    ALONE
        .ledger
        .set(dir.path().join("ledger"))
        .expect("set once");
    let seams = Seams {
        stage: alone_stage,
        admit_quiet: alone_admit,
        admit_memory: alone_admit,
    };
    let (code, log) = run(dir.path(), Some(book(dir.path())), Admit::Wait, seams);
    assert_eq!(code, crate::exit::PASS, "{log}");
    let lint = "verify tippy lint (-p aterm-gui)";
    let seen = ALONE.take();
    assert_eq!(
        seen.looked
            .iter()
            .find(|(title, _)| title == "tippy lint (-p aterm-gui)")
            .map(|(_, held)| held.clone()),
        Some(vec![(lint.to_owned(), 12 * GIB)])
    );
    let host = if cfg!(target_os = "macos") {
        "this Mac"
    } else {
        "this machine"
    };
    let said = format!(
        "memory: claimed 12 GiB cold: no run of it is kept, and it compiles (16 GiB, at most half \
         of physical memory), admitted alone: it needs 12 GiB and 8 GiB was available, short of \
         memory no claim holds (other work on {host}), so no other memory claim is admitted \
         while it runs"
    );
    assert!(said_whole(&log, &said), "{log}");
}

static NOW: Scratch = Scratch::new();

fn now_admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    NOW.admit(need, mode)
}

fn now_stage(_: &Ctx, spec: &StageSpec) -> Report {
    NOW.stage(spec)
}

/// Under `--admit now` a stage's memory claim is taken at once, however short the machine is:
/// beside a 15 GiB claim this process holds, 20 GiB available leaves 5 for the lint's 12, and it
/// starts within the hang detector, its time line saying how it was admitted. NEGATIVE CONTROL:
/// the same claim under `--admit wait` waits for room, and fails the hang detector by name.
#[test]
fn under_admit_now_a_memory_claim_is_taken_at_once() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    NOW.ledger.set(dir.path().join("ledger")).expect("set once");
    let holder = aterm_capacity::admit_with(
        &NOW.ledger(),
        &machine(),
        Need::new("holder").mem_gib(15),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("the holder's claim");
    let seams = Seams {
        stage: now_stage,
        admit_quiet: now_admit,
        admit_memory: now_admit,
    };
    let (code, log) = run(dir.path(), Some(book(dir.path())), Admit::Now, seams);
    assert_eq!(code, crate::exit::PASS, "{log}");
    assert!(
        log.contains("memory: claimed 12 GiB cold") && log.contains(", admitted by --admit now"),
        "{log}"
    );
    let seen = NOW.take();
    assert_eq!(
        seen.asked
            .iter()
            .map(|(_, _, mode)| *mode)
            .collect::<Vec<_>>(),
        [Admit::Now]
    );
    drop(holder);
}

/// Set, the test binary is [`standin_run_waits_for_memory`], its ledger and markers under this
/// directory.
const STANDIN: &str = "ATERM_VERIFY_MEMORY_INTERRUPT_STANDIN";

/// The stand-in run's admission: [`crate::admit_in`] in the scratch ledger, looking every two
/// minutes (twice the parent's hang detector, so only the run's end ends a wait in time), noting
/// each admission asked for in `admissions` and each claim's first wait line in a
/// `waiting-<n>` file.
fn standin_admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    let dir = PathBuf::from(std::env::var_os(STANDIN).expect("the stand-in's directory"));
    let n = note(&dir.join("admissions"), &need.what);
    let waiting = dir.join(format!("waiting-{n}"));
    crate::admit_in(
        &Ledger::at(dir.join("ledger")),
        &machine(),
        Pace {
            poll: MUST_HAPPEN * 2,
            ..Pace::default()
        },
        need,
        mode,
        &mut |line| {
            if !waiting.exists() {
                std::fs::write(&waiting, line).expect("said waiting");
            }
        },
    )
}

/// Add `line` to the file at `path`: how many lines it held before.
fn note(path: &Path, line: &str) -> usize {
    use std::io::Write as _;
    static LOCK: Mutex<()> = Mutex::new(());
    let _held = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let before = std::fs::read_to_string(path)
        .map(|t| t.lines().count())
        .unwrap_or(0);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("a marker file");
    writeln!(f, "{line}").expect("noted");
    before
}

/// The stand-in run's stages: each notes in `ran` that it ran.
fn standin_stage(_: &Ctx, spec: &StageSpec) -> Report {
    let dir = PathBuf::from(std::env::var_os(STANDIN).expect("the stand-in's directory"));
    note(&dir.join("ran"), &spec.title);
    let mut r = Report::new(spec.title.clone());
    r.pass("ran");
    r
}

/// Run by [`one_interrupt_in_two_memory_waits_ends_the_run`] in a process of its own: a verify
/// run of `test` and `lint` under `--admit wait`, its test compile and lint claiming 8.4 and 10.8
/// GiB (19.2 of the run's 21.5 together, so both wait in the ledger at once), its interrupt
/// handling verify's own, that exits as verify does.
#[test]
fn standin_run_waits_for_memory() {
    let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
        return;
    };
    crate::exec::group::kill_on_interrupt();
    let scratch = dir.join("scratch");
    std::fs::create_dir_all(&scratch).expect("the run's scratch");
    let b = book(&dir);
    keep(&b, &dir, StageId::TestCompile, 7);
    keep(&b, &dir, StageId::Tippy, 9);
    let ctx = Ctx::new(dir.clone(), gui(), EnvSnapshot::default(), scratch)
        .with_checks(&[Check::Test, Check::Lint])
        .with_quiet(Some(Admit::Wait))
        .with_memory(b);
    let mut out = std::io::stdout();
    let code = crate::run_with(
        &ctx,
        &mut out,
        &mut std::io::sink(),
        &Show::default(),
        Seams {
            stage: standin_stage,
            admit_quiet: standin_admit,
            admit_memory: standin_admit,
        },
    )
    .expect("the result written");
    std::process::exit(code);
}

/// A stand-in, killed when it is dropped still running (a test that failed before it ended),
/// so none outlives its test.
struct Standin(aterm_uds::spawnfd::Child);

impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// One SIGTERM sent while two stages' memory claims wait in the ledger (behind a 15 GiB claim
/// this test holds, 20 GiB available leaves 5 for 8.4 and 10.8) ends the whole run within the
/// hang detector: exit 3, COULD NOT RUN, no FAIL, the interrupt named; the wait that did not
/// take the signal ends with the run, no stage ran, the test run never asked to start, and only
/// this test's claim is left in the ledger. NEGATIVE CONTROL: a wait that does not look at the
/// run's end keeps waiting after the other took the signal, and the run fails the hang detector
/// by name.
#[test]
fn one_interrupt_in_two_memory_waits_ends_the_run() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-mem-int-")
        .tempdir()
        .expect("a scratch dir");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let held = aterm_capacity::admit_with(
        &ledger,
        &machine(),
        Need::new("holder").mem_gib(15),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("the holder's claim");
    let mut child = Standin(
        aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "memory_claim_tests::standin_run_waits_for_memory",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(STANDIN, dir.path())
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the stand-in run"),
    );
    until_within(MUST_HAPPEN, "both memory claims waiting", || {
        (dir.path().join("waiting-0").exists() && dir.path().join("waiting-1").exists())
            .then_some(())
    });
    let pid = i32::try_from(child.0.id()).expect("a pid");
    // SAFETY: `kill` takes two integers; the pid is this test's own child.
    assert_eq!(
        unsafe { libc::kill(pid, libc::SIGTERM) },
        0,
        "signalled the run"
    );
    let status = until_within(MUST_HAPPEN, "the run's end", || {
        child.0.try_wait().ok().flatten()
    });
    let mut out = String::new();
    if let Some(mut o) = child.0.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut o, &mut out);
    }
    assert_eq!(status.code(), Some(3), "{status}\n{out}");
    assert!(out.contains("verify: COULD NOT RUN"), "{out}");
    assert!(!out.contains("FAIL"), "a finding:\n{out}");
    assert!(
        out.contains("interrupted (SIGTERM) while it waited"),
        "names the wait:\n{out}"
    );
    let asked = std::fs::read_to_string(dir.path().join("admissions")).unwrap_or_default();
    assert_eq!(asked.lines().count(), 2, "two waits: {asked}");
    assert!(
        !dir.path().join("ran").exists(),
        "a stage ran after the interrupt"
    );
    let left: Vec<String> = ledger
        .dir()
        .read_dir()
        .expect("the ledger")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".claim") || n.ends_with(".wait"))
        .collect();
    assert_eq!(left.len(), 1, "only the holder's claim: {left:?}");
    drop(held);
}

/// Set, the test binary is [`standin_appkit_run`] in the role it names (`r` or `s`), under the
/// directory [`APPKIT_DIR`] names.
const APPKIT_ROLE: &str = "ATERM_VERIFY_APPKIT_ROLE";

/// The appkit stand-ins' shared directory: their ledger, books, marks and R's FIFO.
const APPKIT_DIR: &str = "ATERM_VERIFY_APPKIT_DIR";

/// The appkit stand-in's role and shared directory.
fn appkit() -> (String, PathBuf) {
    (
        std::env::var(APPKIT_ROLE).expect("the stand-in's role"),
        PathBuf::from(std::env::var_os(APPKIT_DIR).expect("the stand-ins' directory")),
    )
}

/// The appkit stand-ins' admission: the shared scratch ledger on the 24 GiB machine with 20 GiB
/// available, looking every 20 ms; the first wait line of each kind of claim marks
/// `waiting-<role>-<quiet|memory>`.
fn appkit_admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    let (role, dir) = appkit();
    let kind = if need.quiet.is_some() {
        "quiet"
    } else {
        "memory"
    };
    let mark = dir.join(format!("waiting-{role}-{kind}"));
    crate::admit_in(
        &Ledger::at(dir.join("ledger")),
        &machine(),
        Pace {
            poll: std::time::Duration::from_millis(20),
            ..Pace::default()
        },
        need,
        mode,
        &mut |line| {
            if !mark.exists() {
                std::fs::write(&mark, line).expect("said waiting");
            }
        },
    )
}

/// The appkit stand-ins' stages: R's redraw opens the FIFO `r-go` (read and write, so the open
/// waits for nobody), marks `r-redraw`, and ends once a byte comes through it; every other
/// stage passes at once.
fn appkit_stage(_: &Ctx, spec: &StageSpec) -> Report {
    use std::io::Read as _;
    let (role, dir) = appkit();
    if role == "r" && spec.id == StageId::RedrawConformance {
        let mut go = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("r-go"))
            .expect("the FIFO");
        std::fs::write(dir.join("r-redraw"), "running").expect("marked");
        let mut byte = [0u8; 1];
        let _ = go.read(&mut byte);
    }
    let mut r = Report::new(spec.title.clone());
    r.pass("ran");
    r
}

/// Run by [`a_timed_stage_claims_memory_only_on_its_runs_quiet_machine`] in a process of its own:
/// an appkit run (the redraw conformance, then the objc drives, both timed) under `--admit
/// wait`, on its own book: R's redraw claims 4 GiB and its drives 12, S's redraw 12 and its
/// drives 2. It exits as verify does.
#[test]
fn standin_appkit_run() {
    let Ok(role) = std::env::var(APPKIT_ROLE) else {
        return;
    };
    let (_, dir) = appkit();
    let at = dir.join(&role);
    let scratch = at.join("scratch");
    std::fs::create_dir_all(&scratch).expect("the run's scratch");
    let b = book(&at);
    let (redraw, drives) = if role == "r" { (3, 10) } else { (10, 1) };
    keep(&b, &at, StageId::RedrawConformance, redraw);
    keep(&b, &at, StageId::ObjcDrives, drives);
    let ctx = Ctx::new(at, gui(), EnvSnapshot::default(), scratch)
        .with_checks(&[Check::Appkit])
        .with_quiet(Some(Admit::Wait))
        .with_memory(b);
    let code = crate::run_with(
        &ctx,
        &mut std::io::stdout(),
        &mut std::io::sink(),
        &Show::default(),
        Seams {
            stage: appkit_stage,
            admit_quiet: appkit_admit,
            admit_memory: appkit_admit,
        },
    )
    .expect("the result written");
    std::process::exit(code);
}

/// What a stand-in wrote to its standard output, once it ended.
fn printed(child: &mut Standin) -> String {
    let mut out = String::new();
    if let Some(mut o) = child.0.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut o, &mut out);
    }
    out
}

/// A TIMED STAGE CLAIMS ITS MEMORY ONLY ON ITS RUN'S QUIET MACHINE: run R holds the quiet machine
/// across its redraw (4 GiB) and its drives (12 GiB); run S starts during R's redraw and waits for
/// the quiet machine holding no memory, so once R's redraw ends R's drives are admitted beside
/// nothing of S's (20 GiB available), R ends, and S's redraw (12 GiB) and drives (2 GiB) run on
/// the quiet machine R let go: both runs pass within the hang detector. NEGATIVE CONTROL: with
/// the memory claimed before the quiet machine, as before, S's redraw holds 12 GiB while it waits
/// for R's quiet machine, R's drives wait for that 12 GiB, neither run ever ends, and the hang
/// detector fails by name.
#[cfg(unix)]
#[test]
fn a_timed_stage_claims_memory_only_on_its_runs_quiet_machine() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-appkit-")
        .tempdir()
        .expect("a scratch dir");
    let go = dir.path().join("r-go");
    let c = std::ffi::CString::new(go.as_os_str().as_encoded_bytes()).expect("a path");
    // SAFETY: `mkfifo` reads the NUL-terminated path `c` owns, alive for the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "the FIFO");
    let spawn = |role: &str| {
        Standin(
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "memory_claim_tests::standin_appkit_run",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(APPKIT_ROLE, role)
                .env(APPKIT_DIR, dir.path())
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("a stand-in run"),
        )
    };
    let mut r = spawn("r");
    until_within(MUST_HAPPEN, "R's redraw on its quiet machine", || {
        dir.path().join("r-redraw").exists().then_some(())
    });
    let mut s = spawn("s");
    until_within(MUST_HAPPEN, "S waiting for the quiet machine", || {
        dir.path().join("waiting-s-quiet").exists().then_some(())
    });
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&go)
            .expect("R's redraw reads the FIFO")
            .write_all(b"g")
            .expect("R's redraw let go");
    }
    let (rs, ss) = until_within(MUST_HAPPEN, "both runs to end", || {
        match (r.0.try_wait(), s.0.try_wait()) {
            (Ok(Some(a)), Ok(Some(b))) => Some((a, b)),
            _ => None,
        }
    });
    let (rout, sout) = (printed(&mut r), printed(&mut s));
    assert_eq!(rs.code(), Some(crate::exit::PASS), "R: {rs}\n{rout}");
    assert_eq!(ss.code(), Some(crate::exit::PASS), "S: {ss}\n{sout}");
    assert!(
        !dir.path().join("waiting-r-memory").exists(),
        "R's drives waited for memory:\n{rout}"
    );
}

static MEASURED: Scratch = Scratch::new();

/// What the measured stand-in's lint reports: 0 one passed row, 1 one skipped row, 2 a passed
/// row and a skipped one.
static ROWS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn measured_admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    MEASURED.admit(need, mode)
}

fn measured_stage(_: &Ctx, spec: &StageSpec) -> Report {
    let mut r = Report::new(spec.title.clone());
    match ROWS.load(std::sync::atomic::Ordering::SeqCst) {
        1 => r.skip("skipped"),
        2 => {
            r.pass("ran");
            r.skip("skipped");
        }
        _ => r.pass("ran"),
    }
    r
}

/// A stage's figure is kept only when a row of it passed and none failed, and a 0 only when no
/// row was skipped: a lint whose one row was skipped did none of its work and keeps nothing, and
/// one that passed a row and skipped another, having started no child, keeps nothing either
/// (the skipped row may be the one that starts a child); each says why in its time line.
/// NEGATIVE CONTROL: the lint that passed its one row keeps its 0.
#[test]
fn a_stage_keeps_its_figure_only_when_it_did_its_work() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    MEASURED
        .ledger
        .set(dir.path().join("ledger"))
        .expect("set once");
    let seams = Seams {
        stage: measured_stage,
        admit_quiet: measured_admit,
        admit_memory: measured_admit,
    };
    for (rows, said, kept) in [
        (
            1,
            "memory: peaked nothing: it started no child, not kept: no row of it passed, so it \
             did none of its work",
            false,
        ),
        (
            2,
            "memory: peaked nothing: it started no child, not kept: it skipped a row, which may \
             be the one that starts a child",
            false,
        ),
        (0, "memory: peaked nothing: it started no child", true),
    ] {
        let at = dir.path().join(rows.to_string());
        std::fs::create_dir_all(&at).expect("a folder");
        ROWS.store(rows, std::sync::atomic::Ordering::SeqCst);
        let (_, log) = run(&at, Some(book(&at)), Admit::Wait, seams);
        assert!(said_whole(&log, said), "rows {rows}: {log}");
        let records = crate::memory::Records::load(&at.join("stage-memory.json"));
        assert_eq!(
            records.rows().iter().any(|r| r.key.stage == "Tippy"),
            kept,
            "rows {rows}: {records:?}"
        );
    }
}
